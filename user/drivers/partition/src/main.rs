//! The partition driver.
//!
//! It binds to each `storage:disk` and reads the disk's GUID partition
//! table through the `Block` channel that `Start` gives it as `bus`. For
//! each partition it adds a child device `part-<n>`, where `n` is the
//! entry's 1-based index in the table. Serving the partitions is not
//! there yet: `ConnectChild` and `ResetChild` fail with
//! `NOT_SUPPORTED`. The driver never writes the table.

#![no_std]
#![no_main]

extern crate alloc;

mod log;
mod table;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use mikros_gpt::guid_string;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{ClientEnd, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverRequest, DriverStartRequest,
    DriverStartResponse, DriverStopResponse, Property,
};
use mikros_rt::interfaces::mikros::log::Severity;
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::storage::BlockAsyncClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use table::Table;

mikros_rt::raw_main!(main);

const PARTITION_ID: &str = "storage:partition";
/// The disk and its partitions, read by `Start`.
struct Disk {
    _block: Rc<BlockAsyncClient>,
    partitions: Vec<mikros_gpt::Partition>,
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
    let mut started = false;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(_, responder) if started => {
                    responder.reply(s, Err(Status::BadState))
                }
                DriverRequest::Start(request, responder) => {
                    match start(request, process, &directory, &reactor).await {
                        Ok((disk, manager)) => {
                            started = true;
                            let replied = responder.reply(s, Ok(DriverStartResponse {}));
                            spawner.spawn(add_partitions(disk, manager));
                            replied
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::ResetChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ConnectChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
            };
        }
        0
    })
}

/// Reads the table of the disk behind the `bus` channel of `request`,
/// logs which table it used, and returns the disk and the device manager.
/// Fails with `INVALID_ARGS` without a bus channel, and with
/// `PEER_CLOSED` when the disk's channel failed. A disk without a valid
/// table has no partitions.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
) -> Result<(Disk, DeviceManagerAsyncClient), Status> {
    let bus = request.bus.ok_or(Status::InvalidArgs)?;
    let block = BlockAsyncClient::new(ClientEnd::new(bus), reactor.clone());
    let info = block.get_info().await.map_err(|_| Status::PeerClosed)?.info;
    let read = table::read(&block, &info, process).await;
    let partitions = match read {
        Some((table, partitions)) => {
            let (severity, which) = match table {
                Table::Primary => (Severity::Info, "primary"),
                Table::Backup => (Severity::Warning, "backup"),
            };
            let message = format!(
                "partition: {} has {} partitions in its {which} GPT",
                request.device.name,
                partitions.len()
            );
            log::write(directory, reactor, severity, message).await;
            partitions
        }
        None => {
            let message = format!("partition: {} has no valid GPT", request.device.name);
            log::write(directory, reactor, Severity::Info, message).await;
            Vec::new()
        }
    };
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    let block = Rc::new(block);
    Ok((
        Disk {
            _block: block,
            partitions,
        },
        manager,
    ))
}

/// Adds a device for each partition of `disk`. Keeps the device
/// manager's channel open for as long as the driver runs.
async fn add_partitions(disk: Disk, manager: DeviceManagerAsyncClient) {
    for entry in &disk.partitions {
        let name = format!("part-{}", entry.index);
        let ids = vec![
            format!("gpt:{}", guid_string(&entry.type_guid)),
            PARTITION_ID.into(),
        ];
        let _ = manager
            .add_device(name, ids, properties(entry), vec![])
            .await;
    }
    core::future::pending::<()>().await;
    drop(manager);
}

/// The properties of the device of partition `entry`.
fn properties(entry: &mikros_gpt::Partition) -> Vec<Property> {
    let property = |key: &str, value: String| Property {
        key: key.into(),
        value,
    };
    vec![
        property("partition_guid", guid_string(&entry.guid)),
        property("name", entry.name.clone()),
        property("first_lba", format!("{}", entry.first_lba)),
        property("last_lba", format!("{}", entry.last_lba)),
    ]
}
