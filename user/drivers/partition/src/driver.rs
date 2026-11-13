//! The partition driver.
//!
//! It binds to each `storage:disk` and reads the disk's GUID partition
//! table through the `Block` channel that `Start` gives it as `bus`, or,
//! without a valid one, its master boot record and extended boot
//! records, or, without that either, treats a disk whose start holds a
//! file system as one partition. For each partition it adds a child
//! device `part-<n>`, where `n` is the GPT entry's 1-based index, the
//! MBR partition's number or 0 for the whole disk, matched by its table's
//! type, the file system its first bytes show and `storage:partition`,
//! and publishes the partition's raw access service, of class `storage`. On the bus channel that
//! `ConnectChild` gives it for a partition it serves `Block` for that
//! partition, forwarded to the disk, whose requests outlive the disk
//! driver's restarts: `Reconnect` gives the driver a new channel to the
//! disk, on which they go again (MIKSPEC-0044.R19). `ResetChild` fails
//! with `NOT_SUPPORTED`. The driver never writes the table.

use crate::gpt::{guid_string, mbr};
use crate::link::Link;
use crate::partition::{Access, Partition};
use crate::table::{Layout, Table};
use crate::writers::Writers;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverConnectChildResponse,
    DriverReconnectResponse, DriverRequest, DriverStartRequest, DriverStartResponse,
    DriverStopResponse, Property,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::Provider;
use mikros_rt::interfaces::mikros::storage::{BlockAsyncClient, BlockFlags, BlockInfo};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const PARTITION_ID: &str = "storage:partition";
/// The disk's flags that its partitions report.
const FLAGS: BlockFlags = BlockFlags(
    BlockFlags::READ_ONLY.0 | BlockFlags::FLUSH.0 | BlockFlags::DISCARD.0 | BlockFlags::FUA.0,
);
const SERVICE_CLASS: &str = "storage";

/// The disk's link and its partitions, read by `Start`.
struct Disk {
    link: Rc<Link>,
    partitions: Vec<(Slot, Rc<Partition>)>,
}

/// A partition's device: its number in its name, the hardware ID of its
/// table's type, its blocks, and its properties, with the file system
/// that its first bytes show.
struct Slot {
    number: u32,
    type_id: Option<String>,
    first_lba: u64,
    last_lba: u64,
    name: String,
    partition_guid: String,
    no_automount: bool,
    fs: Option<&'static str>,
}

/// The bit of a GPT entry's attributes that asks for the partition not
/// to be mounted automatically.
const NO_AUTOMOUNT: u64 = 1 << 63;

impl Slot {
    fn gpt(entry: &crate::gpt::Partition) -> Slot {
        Slot {
            number: entry.index,
            type_id: Some(format!("gpt:{}", guid_string(&entry.type_guid))),
            first_lba: entry.first_lba,
            last_lba: entry.last_lba,
            name: entry.name.clone(),
            partition_guid: guid_string(&entry.guid),
            no_automount: entry.attributes & NO_AUTOMOUNT != 0,
            fs: None,
        }
    }

    fn mbr(partition: &mbr::Partition, signature: u32) -> Slot {
        Slot {
            number: partition.number,
            type_id: Some(format!("mbr:{:02x}", partition.kind)),
            first_lba: partition.first_lba,
            last_lba: partition.last_lba,
            name: String::new(),
            partition_guid: partition.identifier(signature),
            no_automount: mbr::is_hidden(partition.kind),
            fs: None,
        }
    }

    /// The device's hardware IDs, in order: its table's type, its file
    /// system, and `storage:partition`.
    fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.type_id.iter().cloned().collect();
        ids.extend(self.fs.map(String::from));
        ids.push(PARTITION_ID.into());
        ids
    }

    /// The device's properties.
    fn properties(&self) -> Vec<Property> {
        let property = |key: &str, value: String| Property {
            key: key.into(),
            value,
        };
        vec![
            property("partition_guid", self.partition_guid.clone()),
            property("name", self.name.clone()),
            property("first_lba", format!("{}", self.first_lba)),
            property("last_lba", format!("{}", self.last_lba)),
            property("no_automount", format!("{}", self.no_automount)),
        ]
    }
}

/// The disk and its partitions' writers, once `Start` has succeeded, the
/// device IDs of the partitions that have been added, and whether adding
/// them is over.
#[derive(Default)]
struct Children {
    disk: RefCell<Option<(Rc<Link>, Rc<Writers>)>>,
    added: RefCell<Vec<(u64, Rc<Partition>)>>,
    done: Cell<bool>,
    waiting: RefCell<Vec<Waker>>,
}

impl Children {
    fn finish(&self) {
        self.done.set(true);
        for waker in self.waiting.borrow_mut().drain(..) {
            waker.wake();
        }
    }

    /// The disk, its partitions' writers and the partition of device `id`,
    /// once adding is over.
    async fn get(&self, id: u64) -> Option<(Rc<Link>, Rc<Writers>, Rc<Partition>)> {
        poll_fn(|cx| match self.done.get() {
            true => Poll::Ready(()),
            false => {
                self.waiting.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;
        let (disk, writers) = self.disk.borrow().clone()?;
        let added = self.added.borrow();
        let partition = added
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, p)| p.clone())?;
        Some((disk, writers, partition))
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
    let mut started = false;
    let children = Rc::new(Children::default());
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(_, responder) if started => {
                    responder.reply(s, Err(Status::BadState))
                }
                DriverRequest::Start(request, responder) => {
                    match start(request, process, &reactor, &spawner).await {
                        Ok((disk, manager)) => {
                            started = true;
                            let writers = Rc::new(Writers::new(disk.link.clone()));
                            *children.disk.borrow_mut() =
                                Some((disk.link.clone(), writers.clone()));
                            let replied = responder.reply(s, Ok(DriverStartResponse {}));
                            let (children, reactor, tasks) =
                                (children.clone(), reactor.clone(), spawner.clone());
                            let added =
                                add_partitions(disk, writers, manager, children, reactor, tasks);
                            spawner.spawn(added);
                            replied
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    // Requests that wait for a new channel fail, and
                    // their replies go out before the driver ends.
                    let link = children
                        .disk
                        .borrow()
                        .as_ref()
                        .map(|(link, _)| link.clone());
                    if let Some(link) = link {
                        link.fail();
                        settle(&reactor).await;
                    }
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::Reconnect(request, responder) => {
                    let link = children
                        .disk
                        .borrow()
                        .as_ref()
                        .map(|(link, _)| link.clone());
                    match link {
                        None => responder.reply(s, Err(Status::BadState)),
                        Some(link) => {
                            let server = server.clone();
                            spawner.spawn(async move {
                                let reconnected = link.reconnect(request.bus).await;
                                let reply = reconnected.map(|()| DriverReconnectResponse {});
                                let _ = responder.reply(server.server(), reply);
                            });
                            Ok(())
                        }
                    }
                }
                DriverRequest::PrepareShutdown(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ResetChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ConnectChild(_, responder) if !started => {
                    responder.reply(s, Err(Status::BadState))
                }
                DriverRequest::ConnectChild(request, responder) => {
                    let (children, server) = (children.clone(), server.clone());
                    let (reactor, spawner2) = (reactor.clone(), spawner.clone());
                    spawner.spawn(async move {
                        let s = server.server();
                        let Some((disk, writers, partition)) = children.get(request.device).await
                        else {
                            let _ = responder.reply(s, Err(Status::NotFound));
                            return;
                        };
                        let _ = responder.reply(s, Ok(DriverConnectChildResponse {}));
                        let (bus, access) = (request.bus, Access::Bus);
                        crate::partition::serve(
                            disk,
                            partition,
                            bus,
                            access,
                            writers,
                            reactor,
                            spawner2,
                            || {},
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

/// Reads the table of the disk behind the `bus` channel of `request`,
/// logs which table it used, finds the file system of each partition,
/// and returns the disk and the device manager. Fails with `INVALID_ARGS`
/// without a bus channel, and with `PEER_CLOSED` when the disk's channel
/// failed. A disk without a valid table and without a file system at its
/// start has no partitions.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<(Disk, DeviceManagerAsyncClient), Status> {
    let bus = request.bus.ok_or(Status::InvalidArgs)?;
    let block = BlockAsyncClient::new(ClientEnd::new(bus), reactor.clone());
    let info = block.get_info().await.map_err(|_| Status::PeerClosed)?.info;
    let name = &request.device.name;
    let mut slots: Vec<Slot> = match crate::table::layout(&block, &info, process).await {
        Layout::Gpt(table, partitions) => {
            let count = partitions.len();
            match table {
                Table::Primary => mikros_rt::log!(
                    Info,
                    "partition: {name} has {count} partitions in its primary GPT"
                ),
                Table::Backup => mikros_rt::log!(
                    Warning,
                    "partition: {name} has {count} partitions in its backup GPT"
                ),
            }
            partitions.iter().map(Slot::gpt).collect()
        }
        Layout::Mbr(signature, partitions) => {
            let count = partitions.len();
            mikros_rt::log!(Info, "partition: {name} has {count} partitions in its MBR");
            partitions.iter().map(|p| Slot::mbr(p, signature)).collect()
        }
        Layout::Whole(found) => {
            mikros_rt::log!(
                Info,
                "partition: {name} has no partition table and holds {} as a whole",
                found.id
            );
            vec![Slot {
                number: 0,
                type_id: None,
                first_lba: 0,
                last_lba: info.block_count - 1,
                name: String::new(),
                partition_guid: found.identifier,
                no_automount: false,
                fs: Some(found.id),
            }]
        }
        Layout::None => {
            mikros_rt::log!(Info, "partition: {name} has no partition table");
            Vec::new()
        }
    };
    let block_size = u64::from(info.block_size);
    for slot in slots.iter_mut().filter(|s| s.fs.is_none()) {
        let bytes = (slot.last_lba - slot.first_lba + 1).saturating_mul(block_size);
        let found = crate::table::find(&block, &info, process, slot.first_lba, bytes).await;
        slot.fs = found.map(|f| f.id);
    }
    let partitions = slots
        .into_iter()
        .map(|slot| {
            let partition = Partition {
                first: slot.first_lba,
                info: BlockInfo {
                    block_count: slot.last_lba - slot.first_lba + 1,
                    flags: BlockFlags(info.flags.0 & FLAGS.0),
                    serial: slot.partition_guid.clone(),
                    ..crate::partition::copy(&info)
                },
            };
            (slot, Rc::new(partition))
        })
        .collect();
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    let link = Link::new(
        Rc::new(block),
        block_size,
        process,
        reactor.clone(),
        spawner.clone(),
    );
    Ok((Disk { link, partitions }, manager))
}

/// Adds a device for each partition of `disk` and publishes its raw
/// access service, which it then serves with the partitions' `writers`.
/// Keeps the device manager's channel open for as long as the driver
/// runs.
async fn add_partitions(
    disk: Disk,
    writers: Rc<Writers>,
    manager: DeviceManagerAsyncClient,
    children: Rc<Children>,
    reactor: Reactor,
    spawner: Spawner,
) {
    for (slot, partition) in &disk.partitions {
        let name = format!("part-{}", slot.number);
        let added = manager
            .add_device(name, slot.ids(), slot.properties(), vec![])
            .await;
        if let Ok(Ok(added)) = added {
            children
                .added
                .borrow_mut()
                .push((added.id, partition.clone()));
        }
    }
    children.finish();
    for (_, partition) in &disk.partitions {
        let Ok((client, server)) = endpoints::<Provider>() else {
            continue;
        };
        if let Ok(Ok(_)) = manager.publish_service(SERVICE_CLASS.into(), client).await {
            spawner.spawn(crate::service::serve_provider(
                disk.link.clone(),
                partition.clone(),
                server,
                writers.clone(),
                reactor.clone(),
                spawner.clone(),
            ));
        }
    }
    core::future::pending::<()>().await;
    drop(manager);
}

/// How long a stopping driver lets the replies of failed requests go.
const SETTLE_NS: i64 = 10_000_000;

/// Lets the driver's other tasks run for `SETTLE_NS`.
async fn settle(reactor: &Reactor) {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    let _ = reactor.sleep_until(now + SETTLE_NS).await;
}
