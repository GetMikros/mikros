//! The USB mass storage driver, for Bulk-Only Transport interfaces with
//! SCSI commands.
//!
//! `Start` takes the interface's bulk IN and bulk OUT endpoints from its
//! `Interface` bus channel and reads the number of logical units. Each
//! unit is probed in a task of its own; one that is a direct-access disk
//! is added, once it is ready, as a child device `disk-<lun>`,
//! `storage:disk`, removable, whose `Block` connections the driver
//! serves, and its raw access service is published with the class
//! `storage`. The buffers of those connections are pinned memory objects
//! that the driver passes to the host process with each transfer.

#![no_std]
#![no_main]

extern crate alloc;

mod bot;
mod lock;
mod unit;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::vec;
use bot::Bot;
use core::cell::RefCell;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverConnectChildResponse, DriverRequest,
    DriverStartRequest, DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::Provider;
use mikros_rt::interfaces::mikros::usb::InterfaceAsyncClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use mikros_storage::{MS, now};
use unit::Unit;

mikros_rt::raw_main!(main);

const KIND_DMA: u32 = 4;
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;
const DISK_ID: &str = "storage:disk";
const SERVICE_CLASS: &str = "storage";
/// Bulk endpoints, from `bmAttributes`.
const BULK: u8 = 2;
/// How long `ConnectChild` waits for a disk that is being added.
const CONNECT_LIMIT: i64 = 5000 * MS;

/// The disks by the IDs of their devices.
type Disks = Rc<RefCell<BTreeMap<u64, Rc<Unit>>>>;

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
    let disks: Disks = Rc::default();
    let mut started = false;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(_, responder) if started => {
                    responder.reply(s, Err(Status::BadState))
                }
                DriverRequest::Start(request, responder) => {
                    match start(request, process, &reactor).await {
                        Ok((bot, dma, serial, manager)) => {
                            started = true;
                            let task = add_disks(
                                bot,
                                dma,
                                serial,
                                manager,
                                disks.clone(),
                                reactor.clone(),
                                spawner.clone(),
                            );
                            spawner.spawn(task);
                            responder.reply(s, Ok(DriverStartResponse {}))
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::Reconnect(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ResetChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ConnectChild(request, responder) => {
                    let (disks, server) = (disks.clone(), server.clone());
                    let (reactor, tasks) = (reactor.clone(), spawner.clone());
                    spawner.spawn(async move {
                        let Some(unit) = disk(&disks, request.device, &reactor).await else {
                            let _ = responder.reply(server.server(), Err(Status::NotFound));
                            return;
                        };
                        let _ = responder.reply(server.server(), Ok(DriverConnectChildResponse {}));
                        let writer = unit.writer.clone();
                        mikros_storage::serve_bus(unit, request.bus, writer, reactor, tasks).await;
                    });
                    Ok(())
                }
            };
        }
        0
    })
}

/// The disk of device `id`, waiting a while for it, since the device
/// manager may ask for it before `AddDevice` returned its ID here.
async fn disk(disks: &Disks, id: u64, reactor: &Reactor) -> Option<Rc<Unit>> {
    let limit = now() + CONNECT_LIMIT;
    loop {
        if let Some(unit) = disks.borrow().get(&id).cloned() {
            return Some(unit);
        }
        if now() >= limit {
            return None;
        }
        let _ = reactor.sleep_until(now() + 10 * MS).await;
    }
}

/// The transport of the interface that `request` gives, with its `DMA`
/// resource, the interface's serial number and the device manager.
/// Fails with `INVALID_ARGS` without a bus channel, a `DMA` resource, or
/// a bulk IN and a bulk OUT endpoint, and with `PEER_CLOSED` when the
/// host process does not answer.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    reactor: &Reactor,
) -> Result<
    (
        Rc<Bot>,
        Rc<OwnedHandle>,
        alloc::string::String,
        DeviceManagerAsyncClient,
    ),
    Status,
> {
    let bus = request.bus.ok_or(Status::InvalidArgs)?;
    let dma = request
        .resources
        .into_iter()
        .find(|r| kind(r) == Some(KIND_DMA))
        .ok_or(Status::InvalidArgs)?;
    let interface = InterfaceAsyncClient::new(ClientEnd::new(bus), reactor.clone());
    let info = interface
        .get_info()
        .await
        .map_err(|_| Status::PeerClosed)?
        .info;
    let bulk = |input: bool| {
        let mut endpoints = info.endpoints.iter();
        let found =
            endpoints.find(|e| e.attributes & 3 == BULK && (e.address & 0x80 != 0) == input);
        found.map(|e| e.address).ok_or(Status::InvalidArgs)
    };
    let (bulk_in, bulk_out) = (bulk(true)?, bulk(false)?);
    let bot = Bot::new(interface, info.number, bulk_in, bulk_out, &dma, process)?;
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    Ok((Rc::new(bot), Rc::new(dma), info.serial, manager))
}

/// Probes each logical unit in a task of its own, and adds those that
/// are disks. Keeps the device manager's channel open for as long as
/// the driver runs.
async fn add_disks(
    bot: Rc<Bot>,
    dma: Rc<OwnedHandle>,
    serial: alloc::string::String,
    manager: DeviceManagerAsyncClient,
    disks: Disks,
    reactor: Reactor,
    spawner: Spawner,
) {
    let units = match bot.units().await {
        Ok(units) => units,
        Err(status) => {
            mikros_rt::log!(Warning, "usb-storage: Get Max LUN failed: {:?}", status);
            return;
        }
    };
    let manager = Rc::new(manager);
    for lun in 0..units {
        let task = add_disk(
            bot.clone(),
            lun,
            dma.clone(),
            serial.clone(),
            manager.clone(),
            disks.clone(),
            reactor.clone(),
            spawner.clone(),
        );
        spawner.spawn(task);
    }
    core::future::pending::<()>().await;
}

/// Probes logical unit `lun`, and when it is a disk adds its device,
/// publishes its raw access service and serves it.
#[allow(clippy::too_many_arguments)]
async fn add_disk(
    bot: Rc<Bot>,
    lun: u8,
    dma: Rc<OwnedHandle>,
    serial: alloc::string::String,
    manager: Rc<DeviceManagerAsyncClient>,
    disks: Disks,
    reactor: Reactor,
    spawner: Spawner,
) {
    let unit = match Unit::probe(bot, lun, dma, serial, &reactor).await {
        Ok(Some(unit)) => Rc::new(unit),
        Ok(None) => return,
        Err(status) => {
            mikros_rt::log!(Warning, "usb-storage: unit {} failed: {:?}", lun, status);
            return;
        }
    };
    let name = format!("disk-{lun}");
    let properties = mikros_storage::disk_properties(&unit.info);
    let added = manager
        .add_device(name, vec![DISK_ID.into()], properties, vec![])
        .await;
    let Ok(Ok(device)) = added else {
        mikros_rt::log!(Warning, "usb-storage: could not add unit {}", lun);
        return;
    };
    disks.borrow_mut().insert(device.id, unit.clone());
    if let Ok((client, server)) = endpoints::<Provider>()
        && let Ok(Ok(_)) = manager.publish_service(SERVICE_CLASS.into(), client).await
    {
        let writer = unit.writer.clone();
        mikros_storage::serve_provider(unit, server, writer, reactor, spawner).await;
    }
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
