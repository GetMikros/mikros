//! The virtio-blk driver.
//!
//! It drives a modern virtio block device through the virtio library,
//! with one request queue. Once `Start` has initialized the device, the
//! driver adds the device's disk as a child device `disk-0`, as
//! `storage:disk`, and serves `Block` for it on the bus channel that
//! `ConnectChild` gives it. It also publishes the disk's raw access
//! service, of class `storage`. `ResetChild` fails with
//! `NOT_SUPPORTED`, and `Stop` resets the device.

#![no_std]
#![no_main]

extern crate alloc;

mod disk;

use alloc::rc::Rc;
use alloc::vec;
use core::cell::Cell;
use core::future::poll_fn;
use core::task::{Poll, Waker};
use disk::Disk;
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
use mikros_virtio::Transport;

mikros_rt::raw_main!(main);

const KIND_DMA: u32 = 4;
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;
/// The disk's device: its name and hardware ID, and the class of its
/// raw access service.
const DISK_NAME: &str = "disk-0";
const DISK_ID: &str = "storage:disk";
const SERVICE_CLASS: &str = "storage";

/// The ID of the disk's device, once it has been added.
#[derive(Default)]
struct DiskDevice {
    id: Cell<Option<u64>>,
    waiting: core::cell::RefCell<alloc::vec::Vec<Waker>>,
}

impl DiskDevice {
    fn set(&self, id: u64) {
        self.id.set(Some(id));
        for waker in self.waiting.borrow_mut().drain(..) {
            waker.wake();
        }
    }

    async fn get(&self) -> u64 {
        poll_fn(|cx| match self.id.get() {
            Some(id) => Poll::Ready(id),
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
    // The driver has no log handle: its messages go to the log service.
    mikros_rt::log::connect(&managed.header.directory);
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
    let mut disk: Option<Rc<Disk>> = None;
    let device = Rc::new(DiskDevice::default());
    let writer = mikros_storage::Writer::default();
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(request, responder) => {
                    match start(request, process, &reactor, &spawner).await {
                        Ok((started, manager)) => {
                            let started = Rc::new(started);
                            disk = Some(started.clone());
                            let replied = responder.reply(s, Ok(DriverStartResponse {}));
                            let (device, writer) = (device.clone(), writer.clone());
                            let (reactor, tasks) = (reactor.clone(), spawner.clone());
                            let added = add_disk(started, manager, device, writer, reactor, tasks);
                            spawner.spawn(added);
                            replied
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    if let Some(disk) = &disk {
                        disk.transport.stop(&reactor).await;
                    }
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                // The driver does not take a new bus channel yet.
                DriverRequest::Reconnect(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ResetChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ConnectChild(request, responder) => {
                    let Some(disk) = disk.clone() else {
                        let _ = responder.reply(s, Err(Status::BadState));
                        continue;
                    };
                    let (device, server, writer) = (device.clone(), server.clone(), writer.clone());
                    let (reactor, spawner2) = (reactor.clone(), spawner.clone());
                    spawner.spawn(async move {
                        if request.device != device.get().await {
                            let _ = responder.reply(server.server(), Err(Status::NotFound));
                            return;
                        }
                        let _ = responder.reply(server.server(), Ok(DriverConnectChildResponse {}));
                        mikros_storage::serve_bus(disk, request.bus, writer, reactor, spawner2)
                            .await;
                    });
                    Ok(())
                }
            };
        }
        0
    })
}

/// Initializes the device that `request` gives, with its `DMA` resource,
/// and reads its disk. Returns the disk and the device manager. Fails
/// with `INVALID_ARGS` without a bus channel or a `DMA` resource, with
/// the status of the bus driver when it refused a request, and with
/// `INTERNAL` when the device could not be initialized.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<(Disk, DeviceManagerAsyncClient), Status> {
    let bus = request.bus.ok_or(Status::InvalidArgs)?;
    let dma = request
        .resources
        .into_iter()
        .find(|r| kind(r) == Some(KIND_DMA));
    let dma = dma.ok_or(Status::InvalidArgs)?;
    let device = DeviceAsyncClient::new(ClientEnd::new(bus), reactor.clone());
    let started =
        Transport::start(device, process, &dma, disk::FEATURES, 1, reactor, spawner).await;
    let started = started.map_err(|e| match e {
        mikros_virtio::Error::Bus(status) => status,
        _ => Status::Internal,
    })?;
    let queue = started.queues[0].clone();
    let disk = Disk::new(started.transport, started.features, queue, dma, process).await?;
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    Ok((disk, manager))
}

/// Adds the disk's device, as `disk-0` and `storage:disk`, and publishes
/// its raw access service, which it then serves with the disk's
/// `writer`. Keeps the device manager's channel open for as long as the
/// driver runs.
async fn add_disk(
    disk: Rc<Disk>,
    manager: DeviceManagerAsyncClient,
    device: Rc<DiskDevice>,
    writer: mikros_storage::Writer,
    reactor: Reactor,
    spawner: Spawner,
) {
    let info = mikros_storage::Disk::info(&*disk);
    let properties = mikros_storage::disk_properties(&info);
    let added = manager
        .add_device(DISK_NAME.into(), vec![DISK_ID.into()], properties, vec![])
        .await;
    if let Ok(Ok(added)) = added {
        device.set(added.id);
    }
    if let Ok((client, server)) = endpoints::<Provider>()
        && let Ok(Ok(_)) = manager.publish_service(SERVICE_CLASS.into(), client).await
    {
        spawner.spawn(mikros_storage::serve_provider(
            disk,
            server,
            writer,
            reactor,
            spawner.clone(),
        ));
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
