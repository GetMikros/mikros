//! The FAT server.
//!
//! It binds to partitions of the EFI system partition and Microsoft
//! basic data types, reads and writes the partition through the `Block`
//! channel that `Start` gives it as `bus`, and publishes the volume as a
//! service of class `filesystem` that serves `mikros.fs.Filesystem`.
//! `Start` fails with `NOT_SUPPORTED` for a partition that does not hold
//! a valid FAT volume. `Stop` writes what remains and marks the volume as
//! shut down cleanly. `Reconnect` gives the partition's new channel after
//! its driver restarted, on which the requests that had not completed go
//! again. `ResetChild` and `ConnectChild` fail with `NOT_SUPPORTED`.
//!
//! This thread serves the `Driver` channel until the volume is mounted,
//! and the volume's clients after that; the driver's thread takes the
//! channel then (see `driver`).

#![no_std]
#![no_main]

extern crate alloc;

mod device;
mod driver;
mod fs;
mod memory;
mod node;
mod time;

use alloc::rc::Rc;
use device::BlockDevice;
use fs::Fs;
use mikros_fat::Volume;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverRequest, DriverServer, DriverStartRequest, DriverStartResponse,
    DriverStopResponse,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{DirectoryClient, Provider};
use mikros_rt::interfaces::mikros::storage::BlockClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const SERVICE_CLASS: &str = "filesystem";
/// The property of the partition that says how its changes may be
/// cached (MIKSPEC-0025.R33), and the value that has them durable at
/// once; any other value, or none, lets them wait for `Sync` or `Stop`.
const CACHE: &str = "cache";
const QUICK_DISCONNECT: &str = "quick-disconnect";
const FOREVER: i64 = i64::MAX;

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
    let server = DriverServer::new(end);
    let process = managed.header.process.raw();
    let directory = DirectoryClient::new(managed.header.directory);
    if !driver::END.init() {
        return PANIC_EXIT_CODE;
    }
    let Some(thread) = driver::Thread::create(process) else {
        return PANIC_EXIT_CODE;
    };
    let fs = loop {
        let Ok(request) = server.next(FOREVER) else {
            return 0;
        };
        let _ = match request {
            DriverRequest::Start(request, responder) => {
                match start(request, process, &directory, &reactor, &spawner) {
                    Ok((fs, manager)) => {
                        let _ = responder.reply(&server, Ok(DriverStartResponse {}));
                        spawner.spawn(publish(fs.clone(), manager));
                        break fs;
                    }
                    Err(status) => responder.reply(&server, Err(status)),
                }
            }
            DriverRequest::Stop(_, responder) => {
                let _ = responder.reply(&server, DriverStopResponse {});
                return 0;
            }
            DriverRequest::Reconnect(_, responder) => {
                responder.reply(&server, Err(Status::BadState))
            }
            DriverRequest::ResetChild(_, responder) => {
                responder.reply(&server, Err(Status::NotSupported))
            }
            DriverRequest::ConnectChild(_, responder) => {
                responder.reply(&server, Err(Status::NotSupported))
            }
        };
    };
    if !thread.start(server, process) {
        mikros_rt::log!(Error, "fat: the driver's thread did not start");
        return PANIC_EXIT_CODE;
    }
    executor.block_on(async {
        let _ = reactor.signals(driver::END.event(), driver::ENDING).await;
    });
    if driver::END.how() == driver::STOPPED {
        fs.unmount();
        driver::END.unmounted();
    }
    0
}

/// Mounts the volume on the partition behind the `bus` channel of
/// `request`, for writing unless the disk is read-only, and returns it
/// and the device manager. Fails with `INVALID_ARGS` without a bus
/// channel, with `NO_RESOURCES` when the disk gives no buffer or there
/// is no pager, with `NOT_SUPPORTED` when the partition holds no valid
/// FAT volume, and with `INTERNAL` when reading it fails.
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
    let device = BlockDevice::new(block, process, &driver::GATE).ok_or(Status::NoResources)?;
    let (size, writable) = (device.size(), !device.read_only());
    let mounted = match quick_disconnect {
        true => Volume::mount_quick_disconnect(device, size, writable),
        false => Volume::mount(device, size, writable),
    };
    let volume = mounted.map_err(|e| match e {
        mikros_fat::Error::Invalid => Status::NotSupported,
        _ => Status::Internal,
    })?;
    let utc = time::Utc::new(directory, process);
    let pager = memory::pager().ok_or(Status::NoResources)?;
    let fs = Rc::new(Fs::new(
        volume,
        utc,
        pager,
        quick_disconnect,
        reactor.clone(),
        spawner.clone(),
    ));
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    Ok((fs, manager))
}

/// Publishes the volume as a `filesystem` service and serves it. Keeps
/// the device manager's channel open for as long as the driver runs.
async fn publish(fs: Rc<Fs>, manager: DeviceManagerAsyncClient) {
    if let Ok((client, server)) = endpoints::<Provider>()
        && let Ok(Ok(_)) = manager.publish_service(SERVICE_CLASS.into(), client).await
    {
        fs.spawner.spawn(fs::serve_provider(fs.clone(), server));
    }
    core::future::pending::<()>().await;
    drop(manager);
}
