//! The ACPI server.
//!
//! It drives the `platform:acpi` device. `Start` takes the address of the
//! RSDP from the device's `rsdp` property, readies ACPICA's operating
//! system layer with resources from the device manager's
//! `RequestResource`, and starts ACPICA. The server then adds the devices
//! of the namespace, as `devices` says, and publishes the `acpi` service
//! class. When the `Driver` channel closes once it has started,
//! as when the device manager exits at shutdown, the server goes on
//! serving its connections. `ConnectChild` for a device the server added
//! succeeds and keeps the channel, on which nothing is served; the
//! server has no bus protocol for its children.

#![no_std]
#![no_main]

extern crate alloc;

mod devices;
mod host;
mod service;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use host::Host;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DriverAsyncServer, DriverConnectChildResponse, DriverRequest, DriverStartRequest,
    DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{DirectoryAsyncClient, Provider};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use service::Acpi;

mikros_rt::raw_main!(main);

const FOREVER: i64 = i64::MAX;
const BROKER: &str = "mikros.permissions.Broker";
/// The service class that the server publishes.
const CLASS: &str = "acpi";
/// The property of the RSDP's address.
const RSDP: &str = "rsdp";

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
    let mut directory = Some(DirectoryAsyncClient::new(
        managed.header.directory,
        reactor.clone(),
    ));
    let mut started: Option<Started> = None;
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
                        Ok(now) => {
                            started = Some(now);
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
                DriverRequest::ConnectChild(request, responder) => match started.as_mut() {
                    Some(started) if started.children.contains(&request.device) => {
                        started.buses.push(request.bus);
                        responder.reply(s, Ok(DriverConnectChildResponse {}))
                    }
                    _ => responder.reply(s, Err(Status::NotFound)),
                },
            };
        }
        // The device manager exits at shutdown without stopping the
        // server, whose connections, the service manager's among them,
        // are served until the machine is powered off or restarted.
        if started.is_some() {
            core::future::pending::<()>().await;
        }
        PANIC_EXIT_CODE
    })
}

/// The server once it started: the devices it added, and the channels
/// that `ConnectChild` gave it for them.
struct Started {
    children: Vec<u64>,
    buses: Vec<OwnedHandle>,
}

async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    directory: DirectoryAsyncClient,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<Started, Status> {
    let rsdp = request
        .device
        .properties
        .iter()
        .find(|p| p.key == RSDP)
        .and_then(|p| host::rsdp(&p.value));
    let Some(rsdp) = rsdp else {
        mikros_rt::log!(Error, "acpi: the device has no valid rsdp property");
        return Err(Status::InvalidArgs);
    };
    let host: &'static Host = Box::leak(Box::new(Host::new(rsdp, request.manager)));
    if !mikros_acpica::install(host, process) {
        return Err(Status::NoResources);
    }
    if let Err(failed) = mikros_acpica::start() {
        mikros_rt::log!(Error, "acpi: starting ACPICA failed: {failed:#x}");
        return Err(service::status(failed));
    }
    let acpi = Rc::new(Acpi::new(broker(&directory, reactor).await));
    service::watch_button(acpi.clone(), reactor.clone(), spawner);
    // The devices come first, as adding them holds up this thread, so
    // that the service is served as soon as it is published.
    let children = devices::add(host);
    let (client, server) = endpoints::<Provider>().map_err(|_| Status::NoResources)?;
    let published = host
        .manager()
        .publish_service(CLASS.into(), client, FOREVER);
    if !matches!(published, Ok(Ok(_))) {
        mikros_rt::log!(Error, "acpi: publishing the acpi service failed");
        return Err(Status::Internal);
    }
    let task = service::serve_provider(acpi, server, reactor.clone(), spawner.clone());
    spawner.spawn(task);
    Ok(Started {
        children,
        buses: Vec::new(),
    })
}

/// The permission broker, through `directory`: None when it cannot be
/// reached, and then no client may power the machine off or restart it.
async fn broker(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<BrokerAsyncClient> {
    let (client, server) = endpoints::<Broker>().ok()?;
    let connected = directory
        .connect(BROKER.into(), None, server.into_handle())
        .await;
    matches!(connected, Ok(Ok(_))).then(|| BrokerAsyncClient::new(client, reactor.clone()))
}
