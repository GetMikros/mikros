//! The CMOS real-time clock driver.
//!
//! It drives `acpi:pnp0b00`, `acpi:pnp0b01` and `acpi:pnp0b02` devices.
//! `Start` takes the clock's index and data ports, the first two of the
//! device's `IOPORT` resource, and the index of its century register from
//! the device's `century` property, when it has one, and publishes the
//! service class `rtc`, which serves `mikros.time.Rtc`: the clock's date
//! and time as it keeps them, in UTC or local time, which the time service
//! decides.

mod clock;
use crate::driver::clock::Clock;
use alloc::rc::Rc;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::hardware::IoPorts;
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverReconnectResponse, DriverRequest,
    DriverStartRequest, DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{Provider, ProviderAsyncServer, ProviderRequest};
use mikros_rt::interfaces::mikros::time::{
    Rtc, RtcAsyncServer, RtcGetResponse, RtcRequest, RtcSetResponse,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const KIND_IOPORT: u32 = 2;
/// The service class that the driver publishes, and its protocol in the
/// service directory.
const CLASS: &str = "rtc";
const PROTOCOL: &str = "device.rtc";
/// The property with the index of the century register.
const CENTURY: &str = "century";

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
    // The clock, which holds its ports' resource while the driver runs,
    // and the bus channel of the ACPI server, which the driver does not
    // use but holds, and which `Reconnect` replaces.
    let mut clock: Option<Rc<Clock>> = None;
    let mut bus: Option<OwnedHandle> = None;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(mut request, responder) => {
                    if clock.is_some() {
                        let _ = responder.reply(s, Err(Status::BadState));
                        continue;
                    }
                    let channel = request.bus.take();
                    match start(request, process, &reactor, &spawner).await {
                        Ok(started) => {
                            clock = Some(started);
                            bus = channel;
                            responder.reply(s, Ok(DriverStartResponse {}))
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                // A restarted ACPI server gives a new bus channel.
                DriverRequest::Reconnect(request, responder) => match clock {
                    Some(_) => {
                        bus = Some(request.bus);
                        responder.reply(s, Ok(DriverReconnectResponse {}))
                    }
                    None => responder.reply(s, Err(Status::BadState)),
                },
                DriverRequest::PrepareShutdown(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ResetChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ConnectChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
            };
        }
        PANIC_EXIT_CODE
    })
}

/// The index of a `century` property: `0x` and lowercase hexadecimal
/// digits.
fn century(value: &str) -> Option<u8> {
    let digits = value.strip_prefix("0x")?;
    let lowercase = digits
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if digits.is_empty() || !lowercase {
        return None;
    }
    u8::from_str_radix(digits, 16).ok()
}

/// Enables the clock's ports and publishes its service: the clock.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<Rc<Clock>, Status> {
    let century = match request.device.properties.iter().find(|p| p.key == CENTURY) {
        Some(p) => Some(century(&p.value).ok_or(Status::InvalidArgs)?),
        None => None,
    };
    let mut resources = request.resources.into_iter();
    let (resource, base) = loop {
        let Some(resource) = resources.next() else {
            mikros_rt::log!(Error, "rtc: the device has no I/O ports");
            return Err(Status::InvalidArgs);
        };
        match mikros_rt::hardware::resource(resource.raw()) {
            Ok(r) if r.kind == KIND_IOPORT && r.size >= 2 && r.base < 0xFFFF => {
                break (resource, r.base);
            }
            _ => {}
        }
    };
    let ports = IoPorts::enable(process, resource, base as u16, 2)
        .map_err(|s| Status::from_raw(s as i32))?;
    let clock = Rc::new(Clock::new(ports, century));
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    let (client, server) = endpoints::<Provider>().map_err(|_| Status::NoResources)?;
    match manager.publish_service(CLASS.into(), client).await {
        Ok(Ok(_)) => {}
        Ok(Err(status)) => return Err(status),
        Err(_) => return Err(Status::PeerClosed),
    }
    spawner.spawn(serve_provider(
        clock.clone(),
        server,
        reactor.clone(),
        spawner.clone(),
        manager,
    ));
    Ok(clock)
}

/// Serves the connections that `end` passes on to the driver. The device
/// manager's client is kept, since closing it would end the driver's
/// place with the device manager.
async fn serve_provider(
    clock: Rc<Clock>,
    end: ServerEnd<Provider>,
    reactor: Reactor,
    spawner: Spawner,
    _manager: DeviceManagerAsyncClient,
) {
    let provider = ProviderAsyncServer::new(end, reactor.clone());
    while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
        // Dropping the server end of another protocol closes it.
        if c.protocol == PROTOCOL {
            let end = ServerEnd::<Rtc>::new(c.server);
            spawner.spawn(serve(clock.clone(), end, reactor.clone()));
        }
    }
}

async fn serve(clock: Rc<Clock>, end: ServerEnd<Rtc>, reactor: Reactor) {
    let server = RtcAsyncServer::new(end, reactor);
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            RtcRequest::Get(_, responder) => {
                responder.reply(s, clock.get().map(|utc| RtcGetResponse { utc }))
            }
            RtcRequest::Set(set, responder) => {
                responder.reply(s, clock.set(set.utc).map(|()| RtcSetResponse {}))
            }
        };
    }
}
