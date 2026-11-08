//! The NVMe driver.
//!
//! `Start` brings the controller up with one admin and one I/O queue
//! pair, and adds each active namespace with 512- or 4096-byte blocks
//! and no metadata as a child device `disk-<nsid>`, `storage:disk`. It
//! serves `Block` for a namespace on the bus channel that `ConnectChild`
//! gives it, and publishes each namespace's raw access service, of class
//! `storage`. `ResetChild` fails with `NOT_SUPPORTED`, and `Stop` shuts
//! the controller down.

#![no_std]
#![no_main]

extern crate alloc;

mod controller;
mod log;
mod namespace;
mod queue;
mod regs;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use controller::Controller;
use core::cell::RefCell;
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverConnectChildResponse,
    DriverReconnectResponse, DriverRequest, DriverStartRequest, DriverStartResponse,
    DriverStopResponse,
};
use mikros_rt::interfaces::mikros::log::Severity;
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{DirectoryAsyncClient, Provider};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use mikros_storage::PciBus;
use namespace::Namespace;

mikros_rt::raw_main!(main);

const KIND_DMA: u32 = 4;
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;
const DISK_ID: &str = "storage:disk";
const SERVICE_CLASS: &str = "storage";

/// The namespaces by the IDs of their devices, once all are added.
#[derive(Default)]
struct Devices {
    added: RefCell<Option<BTreeMap<u64, Rc<Namespace>>>>,
    waiting: RefCell<Vec<Waker>>,
}

impl Devices {
    fn set(&self, added: BTreeMap<u64, Rc<Namespace>>) {
        *self.added.borrow_mut() = Some(added);
        for waker in self.waiting.borrow_mut().drain(..) {
            waker.wake();
        }
    }

    /// The namespace of device `id`, once the devices are added.
    async fn get(&self, id: u64) -> Option<Rc<Namespace>> {
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
    let directory = DirectoryAsyncClient::new(managed.header.directory, reactor.clone());
    let mut controller: Option<Rc<Controller>> = None;
    let devices = Rc::new(Devices::default());
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(request, responder) => {
                    match start(request, process, &directory, &reactor, &spawner).await {
                        Ok((started, namespaces, manager)) => {
                            controller = Some(started);
                            let replied = responder.reply(s, Ok(DriverStartResponse {}));
                            let task = add_disks(
                                namespaces,
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
                    if let Some(controller) = &controller {
                        controller.shutdown(&reactor).await;
                    }
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                // A restarted PCI bus driver gives a new bus channel.
                DriverRequest::Reconnect(request, responder) => match &controller {
                    Some(controller) => {
                        controller.pci.reconnect(request.bus);
                        responder.reply(s, Ok(DriverReconnectResponse {}))
                    }
                    None => responder.reply(s, Err(Status::BadState)),
                },
                DriverRequest::ResetChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ConnectChild(request, responder) => {
                    if controller.is_none() {
                        let _ = responder.reply(s, Err(Status::BadState));
                        continue;
                    }
                    let (devices, server) = (devices.clone(), server.clone());
                    let (reactor, spawner2) = (reactor.clone(), spawner.clone());
                    spawner.spawn(async move {
                        let Some(namespace) = devices.get(request.device).await else {
                            let _ = responder.reply(server.server(), Err(Status::NotFound));
                            return;
                        };
                        let _ = responder.reply(server.server(), Ok(DriverConnectChildResponse {}));
                        let writer = namespace.writer.clone();
                        mikros_storage::serve_bus(
                            namespace,
                            request.bus,
                            writer,
                            reactor,
                            spawner2,
                        )
                        .await;
                    });
                    Ok(())
                }
            };
        }
        0
    })
}

/// Brings up the controller that `request` gives, with its `DMA`
/// resource, and finds its namespaces. Returns the controller, the
/// namespaces it serves and the device manager. Fails with
/// `INVALID_ARGS` without a bus channel or a `DMA` resource, with the
/// status of the bus driver when it refused a request, and with
/// `INTERNAL` when the controller could not be brought up. Skipped
/// namespaces are logged through `directory`.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<(Rc<Controller>, Vec<Rc<Namespace>>, DeviceManagerAsyncClient), Status> {
    let bus = request.bus.ok_or(Status::InvalidArgs)?;
    let dma = request
        .resources
        .into_iter()
        .find(|r| kind(r) == Some(KIND_DMA))
        .ok_or(Status::InvalidArgs)?;
    let pci = PciBus::new(bus, reactor);
    let controller = Controller::start(pci, process, dma, reactor, spawner).await?;
    let mut namespaces = Vec::new();
    for info in controller.namespaces().await? {
        let usable = matches!(info.block_size, 512 | 4096) && info.metadata == 0;
        if !usable {
            let message = format!(
                "nvme: skipping namespace {}: {}-byte blocks with {} bytes of metadata",
                info.nsid, info.block_size, info.metadata
            );
            log::write(directory, reactor, Severity::Info, message).await;
            continue;
        }
        namespaces.push(Rc::new(Namespace {
            nsid: info.nsid,
            block_size: info.block_size as u32,
            blocks: info.blocks,
            controller: controller.clone(),
            writer: mikros_storage::Writer::default(),
        }));
    }
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    Ok((controller, namespaces, manager))
}

/// Adds each namespace's device, as `disk-<nsid>` and `storage:disk`,
/// and publishes and serves its raw access service. Keeps the device
/// manager's channel open for as long as the driver runs.
async fn add_disks(
    namespaces: Vec<Rc<Namespace>>,
    manager: DeviceManagerAsyncClient,
    devices: Rc<Devices>,
    reactor: Reactor,
    spawner: Spawner,
) {
    let mut added = BTreeMap::new();
    for namespace in &namespaces {
        let name = format!("disk-{}", namespace.nsid);
        let info = mikros_storage::Disk::info(&**namespace);
        let properties = mikros_storage::disk_properties(&info);
        let device = manager
            .add_device(name, vec![DISK_ID.into()], properties, vec![])
            .await;
        if let Ok(Ok(device)) = device {
            added.insert(device.id, namespace.clone());
        }
    }
    devices.set(added);
    for namespace in namespaces {
        if let Ok((client, server)) = endpoints::<Provider>()
            && let Ok(Ok(_)) = manager.publish_service(SERVICE_CLASS.into(), client).await
        {
            let writer = namespace.writer.clone();
            let (reactor, tasks) = (reactor.clone(), spawner.clone());
            let task = mikros_storage::serve_provider(namespace, server, writer, reactor, tasks);
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
