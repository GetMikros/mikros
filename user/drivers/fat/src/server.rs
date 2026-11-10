//! The server: it binds to a partition, reads and writes it through the
//! `Block` channel that `Start` gives it as `bus`, and publishes the
//! volume as a service of class `filesystem` that serves
//! `mikros.fs.Filesystem`. `Start` fails with `NOT_SUPPORTED` for a
//! partition that does not hold a valid volume of the format it serves.
//! `Stop` writes what remains and marks the volume as shut down cleanly.
//! `Reconnect` gives the partition's new channel after its driver
//! restarted, on which the requests that had not completed go again.
//! `ResetChild` and `ConnectChild` fail with `NOT_SUPPORTED`.
//!
//! This thread serves the `Driver` channel until the volume is mounted,
//! and the volume's clients after that; the driver's thread takes the
//! channel then (see `driver`).

use crate::device::BlockDevice;
use crate::format::Format;
use crate::fs::{self, Fs};
use crate::{driver, memory, time};
use alloc::rc::Rc;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverRequest, DriverServer, DriverStartRequest, DriverStartResponse,
    DriverStopResponse,
};
use mikros_rt::interfaces::mikros::fs::NodeClient;
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{DirectoryClient, Provider};
use mikros_rt::interfaces::mikros::storage::BlockClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

const SERVICE_CLASS: &str = "filesystem";
/// The property of the partition that says how its changes may be
/// cached (MIKSPEC-0025.R33), and the value that has them durable at
/// once; any other value, or none, lets them wait for `Sync` or `Stop`.
const CACHE: &str = "cache";
const QUICK_DISCONNECT: &str = "quick-disconnect";
const FOREVER: i64 = i64::MAX;

/// Runs the server of the format `F` in the driver process whose
/// bootstrap channel is `bootstrap`, and gives its exit code.
pub fn run<F: Format>(bootstrap: sys::Handle) -> i64 {
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
    let mut zoneinfo = time::zoneinfo(&mut managed.namespace);
    let fs = loop {
        let Ok(request) = server.next(FOREVER) else {
            return 0;
        };
        let _ = match request {
            DriverRequest::Start(request, responder) => {
                match start::<F>(
                    request,
                    process,
                    &directory,
                    &mut zoneinfo,
                    &reactor,
                    &spawner,
                ) {
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
            DriverRequest::PrepareShutdown(_, responder) => {
                responder.reply(&server, Err(Status::NotSupported))
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
        mikros_rt::log!(Error, "{}: the driver's thread did not start", F::NAME);
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
/// volume of the format `F`, and with `INTERNAL` when reading it fails.
/// The volume's times take the zone files from `zoneinfo`.
fn start<F: Format>(
    request: DriverStartRequest,
    process: sys::Handle,
    directory: &DirectoryClient,
    zoneinfo: &mut Option<NodeClient>,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<(Rc<Fs<F>>, DeviceManagerAsyncClient), Status> {
    let quick_disconnect = request
        .device
        .properties
        .iter()
        .any(|p| p.key == CACHE && p.value == QUICK_DISCONNECT);
    let bus = request.bus.ok_or(Status::InvalidArgs)?;
    let block = BlockClient::new(ClientEnd::new(bus));
    let device = BlockDevice::new(block, process, &driver::GATE).ok_or(Status::NoResources)?;
    let (size, writable) = (device.size(), !device.read_only());
    let volume = F::mount(device, size, writable, quick_disconnect).map_err(|e| match e {
        mikros_fat::Error::Invalid => Status::NotSupported,
        _ => Status::Internal,
    })?;
    let pager = memory::pager(F::NAME).ok_or(Status::NoResources)?;
    let clock = time::Clock::new(F::NAME, directory, process, zoneinfo.take());
    let fs = Rc::new(Fs::new(
        volume,
        clock,
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
async fn publish<F: Format>(fs: Rc<Fs<F>>, manager: DeviceManagerAsyncClient) {
    if let Ok((client, server)) = endpoints::<Provider>()
        && let Ok(Ok(_)) = manager.publish_service(SERVICE_CLASS.into(), client).await
    {
        fs.spawner.spawn(fs::serve_provider(fs.clone(), server));
    }
    core::future::pending::<()>().await;
    drop(manager);
}
