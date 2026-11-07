//! The PCI bus driver.
//!
//! It drives a `platform:pci-host-ecam` device. `Start` maps the host
//! bridge's ECAM range, from the `MMIO` resource it gets, uncached as
//! device memory, and obtains a `BTI` resource for the requester IDs of
//! the bridge's buses with `RequestResource`. The driver then adds each
//! function with a type 0 header as a child device, with its BARs
//! claimed and a bus transaction initiator of its own. For each child,
//! it serves `Device` on the bus channel that `ConnectChild` gives it;
//! `ConnectChild` for a device it did not add fails with `NOT_FOUND`.
//! It holds the INTx lines that the functions use and shares each one
//! between the functions on it. `ResetChild` resets a function before
//! its driver is started again.

#![no_std]
#![no_main]

extern crate alloc;

mod bus;
mod device;
mod ecam;
mod interrupts;
mod reset;

use alloc::rc::Rc;
use bus::{Bus, KIND_BTI, KIND_MMIO};
use ecam::Ecam;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverConnectChildResponse, DriverRequest,
    DriverResetChildResponse, DriverStartRequest, DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;
/// The configuration space of one bus, in an ECAM range.
const BUS_SIZE: u64 = 1 << 20;

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
    let mut directory = Some(DirectoryAsyncClient::new(
        managed.header.directory,
        reactor.clone(),
    ));
    let mut bus: Option<Rc<Bus>> = None;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(request, responder) => {
                    let Some(directory) = directory.take() else {
                        let _ = responder.reply(s, Err(Status::BadState));
                        continue;
                    };
                    match start(request, process, directory, &reactor, &spawner).await {
                        Ok(started) => {
                            let started = Rc::new(started);
                            spawner.spawn(bus::enumerate(started.clone()));
                            bus = Some(started);
                            responder.reply(s, Ok(DriverStartResponse {}))
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::ResetChild(request, responder) => {
                    let Some(bus) = bus.clone() else {
                        let _ = responder.reply(s, Err(Status::BadState));
                        continue;
                    };
                    let server = server.clone();
                    spawner.spawn(async move {
                        let reset = match bus.function(request.device).await {
                            Some(function) => reset::reset(&bus, &function).await,
                            None => Err(Status::NotFound),
                        };
                        let reset = reset.map(|()| DriverResetChildResponse {});
                        let _ = responder.reply(server.server(), reset);
                    });
                    Ok(())
                }
                DriverRequest::ConnectChild(request, responder) => {
                    let Some(bus) = bus.clone() else {
                        let _ = responder.reply(s, Err(Status::BadState));
                        continue;
                    };
                    let server = server.clone();
                    spawner.spawn(async move {
                        let Some(function) = bus.function(request.device).await else {
                            let _ = responder.reply(server.server(), Err(Status::NotFound));
                            return;
                        };
                        let connected = Ok(DriverConnectChildResponse {});
                        let _ = responder.reply(server.server(), connected);
                        device::serve(bus, function, request.bus, request.dma, process).await;
                    });
                    Ok(())
                }
            };
        }
        0
    })
}

/// The buses of the host bridge that `request` describes: its ECAM range
/// mapped into `process` from its `MMIO` resource, and the `BTI` resource
/// for their requester IDs. Fails with `INVALID_ARGS` when a property or
/// the resource is missing.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    directory: DirectoryAsyncClient,
    reactor: &mikros_rt::exec::Reactor,
    spawner: &mikros_rt::exec::Spawner,
) -> Result<Bus, Status> {
    let property = |key: &str| {
        let found = request.device.properties.iter().find(|p| p.key == key);
        found.and_then(|p| p.value.parse::<u64>().ok())
    };
    let ecam_base = request
        .device
        .properties
        .iter()
        .find(|p| p.key == "ecam_base")
        .and_then(|p| p.value.strip_prefix("0x"))
        .and_then(|b| u64::from_str_radix(b, 16).ok());
    let (Some(segment), Some(start), Some(end), Some(ecam_base)) = (
        property("segment"),
        property("bus_start"),
        property("bus_end"),
        ecam_base,
    ) else {
        return Err(Status::InvalidArgs);
    };
    let (Ok(segment), Ok(bus_start), Ok(bus_end)) = (
        u16::try_from(segment),
        u8::try_from(start),
        u8::try_from(end),
    ) else {
        return Err(Status::InvalidArgs);
    };
    let mmio = request
        .resources
        .iter()
        .find(|r| kind(r) == Some(KIND_MMIO));
    let mmio = mmio.ok_or(Status::InvalidArgs)?;
    let base = ecam_base + u64::from(bus_start) * BUS_SIZE;
    let ecam = Ecam::map(process, mmio, base, bus_start, bus_end)
        .map_err(|s| Status::from_raw(s as i32))?;
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    let first = (u64::from(segment) << 16) | (u64::from(bus_start) << 8);
    let count = (u64::from(bus_end) - u64::from(bus_start) + 1) << 8;
    let bti = match manager
        .request_resource(KIND_BTI, first, count, false)
        .await
    {
        Ok(Ok(granted)) => Ok(granted.resource),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::PeerClosed),
    };
    Ok(Bus::new(
        ecam,
        segment,
        manager,
        directory,
        bti,
        reactor.clone(),
        spawner.clone(),
    ))
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
