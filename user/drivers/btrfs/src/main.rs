// SPDX-License-Identifier: GPL-2.0
//! The btrfs server (MIKSPEC-0053.R1, MIKSPEC-0053.R2): Linux's btrfs
//! code on the btrfs shim, which `mikros-btrfs` links.
//!
//! It binds to partitions of the Mikros root file system type, and on
//! `Start` gives the shim the partition behind the `bus` channel, which
//! it reads and writes through `mikros.storage.Block`, mounts the btrfs
//! on it, for writing unless the partition is read-only, with the options
//! of `OPTIONS` (MIKSPEC-0053.R3), and publishes it
//! as a service of class `filesystem` that serves `mikros.fs.Filesystem`
//! (MIKSPEC-0049.R13). `Start` fails with `NOT_SUPPORTED` for a partition
//! that holds no btrfs. Changes are durable when their calls return if
//! the partition's `cache` property is `quick-disconnect`, and are
//! written back otherwise (MIKSPEC-0049.R20). `Stop` writes what has
//! changed to stable storage, flushes the partition and ends the server.
//! The partition's channel outlives the partition driver's restarts:
//! `Reconnect` gives a new one, on which unfinished requests go again
//! (MIKSPEC-0044.R19).
//! `ResetChild` and `ConnectChild` fail with `NOT_SUPPORTED`.

#![no_std]
#![no_main]

extern crate alloc;

mod change;
mod dir;
mod disk;
mod file;
mod fs;
mod host;
mod lock;
mod node;
mod share;
mod subvolumes;
mod time;
mod watch;
mod work;

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::ffi::{CStr, c_void};
use disk::Disk;
use fs::Fs;
use mikros_btrfs::Mount;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverReconnectResponse, DriverRequest,
    DriverStartRequest, DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{DirectoryClient, Provider};
use mikros_rt::interfaces::mikros::storage::BlockClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const EINVAL: i32 = -22;
const SERVICE_CLASS: &str = "filesystem";
/// The class of the service that manages the file system's subvolumes.
const SUBVOLUMES_CLASS: &str = "subvolumes";
/// The options of the mount: the free space tree, which a writable mount
/// builds when the file system has none yet, and data compressed with
/// zstd; and with them, on a partition that takes `Discard`, discarding
/// what is freed in the background.
const OPTIONS: &CStr = c"space_cache=v2,compress=zstd";
const OPTIONS_DISCARD: &CStr = c"space_cache=v2,compress=zstd,discard=async";
/// The property of the partition that says how its changes may be
/// cached (MIKSPEC-0025.R33), and the value that has them durable at
/// once; any other value, or none, lets them be written back.
const CACHE: &str = "cache";
const QUICK_DISCONNECT: &str = "quick-disconnect";

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
    let server = DriverAsyncServer::new(end, reactor.clone());
    let process = managed.header.process.raw();
    let directory = DirectoryClient::new(managed.header.directory);
    let mut served: Option<Rc<Fs>> = None;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(_, responder) if served.is_some() => {
                    responder.reply(s, Err(Status::BadState))
                }
                DriverRequest::Start(request, responder) => {
                    match start(request, process, &directory, &reactor, &spawner) {
                        Ok((fs, manager)) => {
                            served = Some(fs.clone());
                            let replied = responder.reply(s, Ok(DriverStartResponse {}));
                            spawner.spawn(harvest(fs.clone()));
                            spawner.spawn(publish(fs, manager));
                            replied
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    if let Some(fs) = &served {
                        fs.write_out();
                    }
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::Reconnect(request, responder) => match &served {
                    Some(fs) => responder.reply(
                        s,
                        fs.reconnect(request.bus, process)
                            .map(|()| DriverReconnectResponse {}),
                    ),
                    None => responder.reply(s, Err(Status::BadState)),
                },
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

/// How much memory the shim may hold: a quarter of the machine's, which
/// sizes its hash tables and the page cache's dirty limits.
fn memory() -> u64 {
    let mut record = [0u8; 16];
    // SAFETY: the record holds the 16 bytes the call writes.
    unsafe { sys::system_get_memory_info(record.as_mut_ptr()) };
    let frames = u64::from_le_bytes(record[..8].try_into().unwrap());
    (frames * 4096 / 4).max(64 << 20)
}

/// Mounts the btrfs on the partition behind the `bus` channel of
/// `request`, and returns the file system to serve and the device
/// manager. Fails with `INVALID_ARGS` without a bus channel, with
/// `NO_RESOURCES` when the disk gives no buffer, with `NOT_SUPPORTED`
/// when the partition holds no btrfs, and with `INTERNAL` when the shim
/// cannot start or mount it.
fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    directory: &DirectoryClient,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<(Rc<Fs>, DeviceManagerAsyncClient), Status> {
    let quick_disconnect = request
        .device
        .properties
        .iter()
        .any(|p| p.key == CACHE && p.value == QUICK_DISCONNECT);
    let bus = request.bus.ok_or(Status::InvalidArgs)?;
    let block = BlockClient::new(ClientEnd::new(bus));
    let disk = Disk::new(block, process).ok_or(Status::NoResources)?;
    host::init(
        process,
        memory(),
        time::page(directory, process).unwrap_or(0),
    );
    mikros_btrfs::start().map_err(|_| Status::Internal)?;
    // The shim keeps the disk for the rest of the program.
    let disk: &'static Disk = Box::leak(Box::new(disk));
    let read_only = disk.read_only();
    let handle = disk as *const Disk as *mut c_void;
    // SAFETY: the handle points to a disk that is never freed, which the
    // host layer's disk calls take.
    unsafe {
        mikros_btrfs::add_disk(
            c"partition",
            handle,
            disk.size(),
            disk.info.block_size,
            read_only,
            disk.discards(),
        )
    }
    .map_err(|_| Status::Internal)?;
    let options = match disk.discards() {
        true => OPTIONS_DISCARD,
        false => OPTIONS,
    };
    let mount = Mount::new(c"/dev/partition", read_only, Some(options)).map_err(|e| match e {
        EINVAL => Status::NotSupported,
        _ => Status::Internal,
    })?;
    // The mount stays for the rest of the program, as its nodes do.
    let mount: &'static Mount = Box::leak(Box::new(mount));
    let fs = Fs::new(
        mount,
        read_only,
        disk,
        quick_disconnect,
        reactor.clone(),
        spawner.clone(),
    )
    .map_err(|_| Status::Internal)?;
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    Ok((Rc::new(fs), manager))
}

/// Publishes the file system as a `filesystem` service and then the
/// management of its subvolumes as a `subvolumes` service
/// (MIKSPEC-0053.R10), and serves them. Keeps the device manager's channel
/// open for as long as the server runs.
async fn publish(fs: Rc<Fs>, manager: DeviceManagerAsyncClient) {
    if let Ok((client, server)) = endpoints::<Provider>()
        && let Ok(Ok(_)) = manager.publish_service(SERVICE_CLASS.into(), client).await
    {
        fs.spawner.spawn(fs::serve_provider(fs.clone(), server));
        if let Ok((client, server)) = endpoints::<Provider>()
            && let Ok(Ok(_)) = manager
                .publish_service(SUBVOLUMES_CLASS.into(), client)
                .await
        {
            fs.spawner
                .spawn(subvolumes::serve_provider(fs.clone(), server));
        }
    }
    core::future::pending::<()>().await;
    drop(manager);
}

/// How often what clients wrote through mappings of memory objects of
/// files is marked dirty in the page cache, so that it is written back.
const HARVEST_PERIOD: i64 = 5_000_000_000;

/// Marks what clients wrote through mappings dirty every
/// `HARVEST_PERIOD`, for as long as the server runs.
async fn harvest(fs: Rc<Fs>) {
    loop {
        let mut now = 0;
        // SAFETY: the out pointer is valid for the call.
        unsafe { sys::clock_get_monotonic(&mut now) };
        if fs.reactor.sleep_until(now + HARVEST_PERIOD).await.is_err() {
            return;
        }
        fs.shares.harvest();
    }
}
