//! The AHCI driver.
//!
//! `Start` takes the HBA over from the firmware, resets it, and sets up
//! and starts each implemented port. It has no child devices yet, so
//! `ConnectChild` fails with `NOT_FOUND`, and `ResetChild` fails with
//! `NOT_SUPPORTED`.

#![no_std]
#![no_main]

extern crate alloc;

mod hba;
mod port;

use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DriverAsyncServer, DriverRequest, DriverStartRequest, DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::pci::DeviceAsyncClient;
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use mikros_storage::Mmio;
use port::Port;

mikros_rt::raw_main!(main);

const KIND_DMA: u32 = 4;
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;
/// The BAR of the HBA's registers.
const ABAR: u8 = 5;
const SIGNALED: u32 = 1 << 3;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let Some(at) = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::Provider)
    else {
        return PANIC_EXIT_CODE;
    };
    let end = ServerEnd::new(managed.handles.remove(at).handle);
    let server = Rc::new(DriverAsyncServer::new(end, reactor.clone()));
    let process = managed.header.process.raw();
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(request, responder) => {
                    let started = start(request, process, &reactor, &spawner).await;
                    responder.reply(s, started.map(|()| DriverStartResponse {}))
                }
                DriverRequest::Stop(_, responder) => {
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::ResetChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ConnectChild(_, responder) => {
                    responder.reply(s, Err(Status::NotFound))
                }
            };
        }
        0
    })
}

fn bus<T>(
    reply: Result<Result<T, Status>, mikros_rt::interfaces::__midl::Error>,
) -> Result<T, Status> {
    match reply {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::Internal),
    }
}

/// Brings up the HBA that `request` gives, with its `DMA` resource.
/// Fails with `INVALID_ARGS` without a bus channel or a `DMA` resource,
/// with the status of the bus driver when it refused a request, and
/// with `INTERNAL` when the HBA could not be brought up.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<(), Status> {
    let bus_end = request.bus.ok_or(Status::InvalidArgs)?;
    let dma = request
        .resources
        .into_iter()
        .find(|r| kind(r) == Some(KIND_DMA))
        .ok_or(Status::InvalidArgs)?;
    let device = Rc::new(DeviceAsyncClient::new(
        ClientEnd::new(bus_end),
        reactor.clone(),
    ));
    let bar = bus(device.get_bar(ABAR).await)?.bar;
    let mmio = Rc::new(Mmio::map(process, &bar.resource, bar.base, bar.size)?);
    bus(device.set_bus_master(true).await)?;
    let bti = bus(device.get_bti().await)?.bti;
    hba::take_over(&mmio, reactor).await;
    hba::reset(&mmio, reactor)
        .await
        .map_err(|_| Status::Internal)?;
    let caps = hba::capabilities(&mmio);
    let below_4g = !caps.addressing_64;
    let mut ports = Vec::new();
    for number in hba::ports(&mmio) {
        let port = Port::new(
            number,
            mmio.clone(),
            caps.slots,
            &dma,
            &bti,
            process,
            below_4g,
        )?;
        port.set_up(reactor, caps.staggered_spin_up)
            .await
            .map_err(|_| Status::Internal)?;
        ports.push(port);
    }
    port::wait_for_links(&ports, reactor).await;
    for port in &ports {
        // A port whose device stays busy is left stopped.
        let _ = port.start(reactor).await;
    }
    let ports: Vec<Rc<Port>> = ports.into_iter().map(Rc::new).collect();
    interrupts(&device, &mmio, &ports, reactor, spawner).await?;
    hba::enable_interrupts(&mmio);
    Ok(())
}

/// Takes the HBA's interrupt through `EnableMsi`, or else the INTx
/// event. On each one, each port that reports something has its `PxIS`
/// and then its bit of `IS` cleared, and the interrupt is then
/// acknowledged. The task keeps the
/// interrupt and the bus channel for as long as the driver runs.
async fn interrupts(
    device: &Rc<DeviceAsyncClient>,
    mmio: &Rc<Mmio>,
    ports: &[Rc<Port>],
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<(), Status> {
    let (handle, msi) = match device.enable_msi(1).await {
        Ok(Ok(enabled)) => match enabled.interrupts.into_iter().next() {
            Some(interrupt) => (interrupt, true),
            None => return Err(Status::Internal),
        },
        _ => (bus(device.get_interrupt().await)?.event, false),
    };
    let raw = handle.raw();
    let (device, mmio, ports) = (device.clone(), mmio.clone(), ports.to_vec());
    let reactor = reactor.clone();
    spawner.spawn(async move {
        let _handle = handle;
        while reactor.signals(raw, SIGNALED).await.is_ok() {
            for port in &ports {
                if port.interrupt() {
                    mmio.write32(hba::IS, 1 << port.number);
                }
            }
            if msi {
                // SAFETY: the call takes no pointers.
                unsafe { sys::interrupt_ack(raw) };
            } else if device.ack_interrupt().await.is_err() {
                return;
            }
        }
    });
    Ok(())
}

/// The kind of `resource`, if it is one.
fn kind(resource: &OwnedHandle) -> Option<u32> {
    let mut info = [0u8; INFO_RESOURCE_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `info` and `len`.
    let status = unsafe {
        sys::object_get_info(
            resource.raw(),
            INFO_RESOURCE,
            info.as_mut_ptr(),
            info.len() as u64,
            &mut len,
        )
    };
    (status == mikros_rt::OK).then(|| u32::from_le_bytes([info[0], info[1], info[2], info[3]]))
}
