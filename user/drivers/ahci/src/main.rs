//! The AHCI driver.
//!
//! `Start` takes the HBA over from the firmware, resets it, sets up and
//! starts each implemented port, and identifies the SATA disks on them.
//! It adds each disk with 48-bit addressing as a child device
//! `disk-<port>`, `storage:disk`, serves `Block` for it on the bus channel
//! that `ConnectChild` gives it, and publishes its raw access service, of
//! class `storage`. `ResetChild` fails with `NOT_SUPPORTED`. `Stop`
//! flushes the disks with a volatile write cache and stops the ports.

#![no_std]
#![no_main]

extern crate alloc;

mod disk;
mod hba;
mod port;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::poll_fn;
use core::task::{Poll, Waker};
use disk::{Disk, Memory};
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverConnectChildResponse, DriverRequest,
    DriverStartRequest, DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::pci::DeviceAsyncClient;
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::Provider;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use mikros_storage::{Mmio, PAGE};
use port::{Event, Port};

mikros_rt::raw_main!(main);

const KIND_DMA: u32 = 4;
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;
/// The BAR of the HBA's registers.
const ABAR: u8 = 5;
const DISK_ID: &str = "storage:disk";
const SERVICE_CLASS: &str = "storage";
const SIGNALED: u32 = 1 << 3;

/// The ports and disks of a started HBA.
type Started = (Vec<Rc<Port>>, Vec<Rc<Disk>>);

/// The disks by the IDs of their devices, once all are added.
#[derive(Default)]
struct Devices {
    added: RefCell<Option<BTreeMap<u64, Rc<Disk>>>>,
    waiting: RefCell<Vec<Waker>>,
}

impl Devices {
    fn set(&self, added: BTreeMap<u64, Rc<Disk>>) {
        *self.added.borrow_mut() = Some(added);
        for waker in self.waiting.borrow_mut().drain(..) {
            waker.wake();
        }
    }

    /// The disk of device `id`, once the devices are added.
    async fn get(&self, id: u64) -> Option<Rc<Disk>> {
        poll_fn(|cx| match &*self.added.borrow() {
            Some(added) => Poll::Ready(added.get(&id).cloned()),
            None => {
                self.waiting.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}

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
    let mut started: Option<Started> = None;
    let devices = Rc::new(Devices::default());
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(request, responder) => {
                    match start(request, process, &reactor, &spawner).await {
                        Ok((ports, disks, manager)) => {
                            started = Some((ports, disks.clone()));
                            let replied = responder.reply(s, Ok(DriverStartResponse {}));
                            let task = add_disks(
                                disks,
                                manager,
                                devices.clone(),
                                reactor.clone(),
                                spawner.clone(),
                            );
                            spawner.spawn(task);
                            replied
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    if let Some((ports, disks)) = &started {
                        stop(ports, disks, &reactor).await;
                    }
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::ResetChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ConnectChild(request, responder) => {
                    if started.is_none() {
                        let _ = responder.reply(s, Err(Status::BadState));
                        continue;
                    }
                    let (devices, server) = (devices.clone(), server.clone());
                    let (reactor, spawner2) = (reactor.clone(), spawner.clone());
                    spawner.spawn(async move {
                        let Some(disk) = devices.get(request.device).await else {
                            let _ = responder.reply(server.server(), Err(Status::NotFound));
                            return;
                        };
                        let _ = responder.reply(server.server(), Ok(DriverConnectChildResponse {}));
                        mikros_storage::serve(disk, request.bus, true, reactor, spawner2, || {})
                            .await;
                    });
                    Ok(())
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

/// Brings up the HBA that `request` gives, with its `DMA` resource, and
/// identifies its disks. Returns the ports, the disks and the device
/// manager. Fails
/// with `INVALID_ARGS` without a bus channel or a `DMA` resource, with
/// the status of the bus driver when it refused a request, and with
/// `INTERNAL` when the HBA could not be brought up.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<(Vec<Rc<Port>>, Vec<Rc<Disk>>, DeviceManagerAsyncClient), Status> {
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
    let memory = Rc::new(Memory {
        dma,
        bti,
        process,
        below_4g: !caps.addressing_64,
    });
    let mut ports = Vec::new();
    for number in hba::ports(&mmio) {
        let port = Port::new(
            number,
            mmio.clone(),
            caps.slots,
            &memory.dma,
            &memory.bti,
            process,
            memory.below_4g,
        )?;
        port.set_up(reactor, caps.staggered_spin_up)
            .await
            .map_err(|_| Status::Internal)?;
        ports.push(port);
    }
    port::wait_for_links(&ports, reactor).await;
    for port in &ports {
        // A port that cannot start has no disk to identify.
        let _ = port.start(reactor).await;
    }
    let ports: Vec<Rc<Port>> = ports.into_iter().map(Rc::new).collect();
    interrupts(
        &device,
        &mmio,
        &ports,
        caps.staggered_spin_up,
        reactor,
        spawner,
    )
    .await?;
    hba::enable_interrupts(&mmio);
    let page = memory.contiguous(PAGE)?;
    let mut disks = Vec::new();
    for port in ports.iter().filter(|p| p.has_disk()) {
        let Ok(identity) = disk::identify(port, &page).await else {
            continue;
        };
        if !identity.lba48 {
            continue;
        }
        let queued = caps.ncq && identity.ncq;
        if queued {
            port.set_depth(identity.queue_depth);
        }
        disks.push(Rc::new(Disk {
            port: port.clone(),
            identity,
            queued,
            memory: memory.clone(),
            spare: RefCell::new(Vec::new()),
        }));
    }
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    Ok((ports, disks, manager))
}

/// Before the driver ends: flushes each disk with a volatile write cache
/// with FLUSH CACHE EXT, waiting for it, and then stops every port.
async fn stop(ports: &[Rc<Port>], disks: &[Rc<Disk>], reactor: &Reactor) {
    for disk in disks.iter().filter(|d| d.identity.write_cache) {
        let _ = disk.flush().await;
    }
    for port in ports {
        let _ = port.stop(reactor).await;
    }
}

/// Takes the HBA's interrupt through `EnableMsi`, or else the INTx
/// event. On each one, each port that reports something has its `PxIS`
/// and then its bit of `IS` cleared, a port that reported an error is
/// recovered, and the interrupt is then acknowledged. The task keeps
/// the interrupt and the bus channel for as long as the driver runs.
async fn interrupts(
    device: &Rc<DeviceAsyncClient>,
    mmio: &Rc<Mmio>,
    ports: &[Rc<Port>],
    spin_up: bool,
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
    let (reactor, tasks) = (reactor.clone(), spawner.clone());
    spawner.spawn(async move {
        let _handle = handle;
        while reactor.signals(raw, SIGNALED).await.is_ok() {
            for port in &ports {
                let event = port.interrupt();
                if matches!(event, Event::None) {
                    continue;
                }
                mmio.write32(hba::IS, 1 << port.number);
                if matches!(event, Event::Error) {
                    let (port, reactor) = (port.clone(), reactor.clone());
                    tasks.spawn(async move { port.recover(&reactor, spin_up).await });
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

/// Adds each disk's device, as `disk-<port>` and `storage:disk`, and
/// publishes and serves its raw access service. Keeps the device
/// manager's channel open for as long as the driver runs.
async fn add_disks(
    disks: Vec<Rc<Disk>>,
    manager: DeviceManagerAsyncClient,
    devices: Rc<Devices>,
    reactor: Reactor,
    spawner: Spawner,
) {
    let mut added = BTreeMap::new();
    for disk in &disks {
        let name = format!("disk-{}", disk.port.number);
        let device = manager
            .add_device(name, vec![DISK_ID.into()], vec![], vec![])
            .await;
        if let Ok(Ok(device)) = device {
            added.insert(device.id, disk.clone());
        }
    }
    devices.set(added);
    for disk in disks {
        if let Ok((client, server)) = endpoints::<Provider>()
            && let Ok(Ok(_)) = manager.publish_service(SERVICE_CLASS.into(), client).await
        {
            let task =
                mikros_storage::serve_provider(disk, server, reactor.clone(), spawner.clone());
            spawner.spawn(task);
        }
    }
    core::future::pending::<()>().await;
    drop(manager);
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
