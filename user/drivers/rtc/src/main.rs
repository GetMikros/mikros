//! The CMOS real-time clock driver.
//!
//! It drives `acpi:pnp0b00`, `acpi:pnp0b01` and `acpi:pnp0b02` devices.
//! `Start` takes the clock's index and data ports, the first two of the
//! device's `IOPORT` resource, and the index of its century register from
//! the device's `century` property, when it has one, and publishes the
//! service class `rtc`, which serves `mikros.time.Rtc`: the clock's date
//! and time as it keeps them, in UTC or local time, which the time service
//! decides.

#![no_std]
#![no_main]

extern crate alloc;

mod clock;

use alloc::rc::Rc;
use clock::Clock;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverRequest, DriverStartRequest,
    DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{Provider, ProviderAsyncServer, ProviderRequest};
use mikros_rt::interfaces::mikros::time::{
    Rtc, RtcAsyncServer, RtcGetResponse, RtcRequest, RtcSetResponse,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const KIND_IOPORT: u32 = 2;
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;
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
    // The ports' resource, which stays held while the driver runs.
    let mut ports: Option<OwnedHandle> = None;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(request, responder) => {
                    if ports.is_some() {
                        let _ = responder.reply(s, Err(Status::BadState));
                        continue;
                    }
                    match start(request, process, &reactor, &spawner).await {
                        Ok(resource) => {
                            ports = Some(resource);
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
                DriverRequest::ConnectChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
            };
        }
        PANIC_EXIT_CODE
    })
}

/// The kind, base and size of `resource`.
fn info(resource: &OwnedHandle) -> Option<(u32, u64, u64)> {
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
    if status != mikros_rt::OK {
        return None;
    }
    let u64_at = |at: usize| u64::from_le_bytes(info[at..at + 8].try_into().unwrap());
    let kind = u32::from_le_bytes(info[0..4].try_into().unwrap());
    Some((kind, u64_at(0x08), u64_at(0x10)))
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

/// Enables the clock's ports and publishes its service: the ports'
/// resource.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<OwnedHandle, Status> {
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
        match info(&resource) {
            Some((KIND_IOPORT, base, size)) if size >= 2 && base < 0xFFFF => {
                break (resource, base);
            }
            _ => {}
        }
    };
    // SAFETY: the call takes no pointers.
    let enabled = unsafe { sys::process_ioports_enable(process, resource.raw(), base, 2) };
    if enabled != mikros_rt::OK {
        return Err(Status::from_raw(enabled as i32));
    }
    let clock = Rc::new(Clock::new(base as u16, century));
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    let (client, server) = endpoints::<Provider>().map_err(|_| Status::NoResources)?;
    match manager.publish_service(CLASS.into(), client).await {
        Ok(Ok(_)) => {}
        Ok(Err(status)) => return Err(status),
        Err(_) => return Err(Status::PeerClosed),
    }
    spawner.spawn(serve_provider(
        clock,
        server,
        reactor.clone(),
        spawner.clone(),
        manager,
    ));
    Ok(resource)
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
