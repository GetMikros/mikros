//! The test drivers of the device manager tests, which do as their app
//! ID says.
//!
//! Each serves `Driver` on its `PROVIDER` startup handle, and `Stop`
//! ends the driver. `Start` depends on the app ID:
//! - As `system.driver.test-host`, a bus driver, it succeeds when the
//!   driver is started for the q35 host bridge, `pci-host-0`, as
//!   `platform:pci-host-ecam`, with its properties, no bus channel, and
//!   an `MMIO` and a `DMA` resource in that order, and fails with
//!   `INVALID_ARGS` otherwise. Once it succeeded, the driver checks that
//!   the device manager refuses to add a device with an invalid hardware
//!   ID, with `INVALID_ARGS`, and to remove its own device, with
//!   `ACCESS_DENIED`, or one that does not exist, with `NOT_FOUND`. It
//!   checks that `RequestResource` gives it a shared `MMIO` resource in
//!   its ECAM range and a `BTI` resource for requester IDs of bus 0, and
//!   refuses a `DMA` resource with `INVALID_ARGS` and a `BTI` resource
//!   for segment 1 with `ACCESS_DENIED`. It then adds a device
//!   `test-child` as `test:child` with a property `slot` of `0`, or
//!   `checks-failed` when a check failed. It accepts
//!   `ConnectChild`, and once the child's driver has written on the bus
//!   channel, it removes the child, when `ConnectChild` said that the
//!   child's driver needs `dma`. `ResetChild` succeeds.
//! - As `system.driver.test-child`, it succeeds when it has a bus
//!   channel and `RequestResource` refuses it, which does not need
//!   `bus`, an `MMIO` resource with `ACCESS_DENIED`, after writing a
//!   message on the bus channel, and fails with `INVALID_ARGS`
//!   otherwise.
//! - As `system.driver.test-decline`, it fails with `NOT_SUPPORTED`.
//! - As `system.driver.test-close`, the driver ends without replying.
//! - As any other, it fails with `INVALID_ARGS`.
//!
//! Drivers other than `test-host` fail `ResetChild` and `ConnectChild`
//! with `NOT_SUPPORTED`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec;
use core::cell::RefCell;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DeviceManagerRequestResourceResponse, DriverAsyncServer,
    DriverConnectChildResponse, DriverRequest, DriverResetChildResponse, DriverStartRequest,
    DriverStartResponse, DriverStopResponse, Property,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const HOST: &str = "system.driver.test-host";
const CHILD: &str = "system.driver.test-child";
const DECLINE: &str = "system.driver.test-decline";
const CLOSE: &str = "system.driver.test-close";
const KIND_MMIO: u32 = 1;
const KIND_DMA: u32 = 4;
const KIND_BTI: u32 = 8;
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;
const READABLE: u32 = 1 << 0;

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
    let app_id = managed.header.app_id.clone();
    let Some(at) = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::Provider)
    else {
        return PANIC_EXIT_CODE;
    };
    let end = ServerEnd::new(managed.handles.remove(at).handle);
    let server = DriverAsyncServer::new(end, reactor.clone());
    let manager: Rc<RefCell<Option<Rc<DeviceManagerAsyncClient>>>> = Rc::default();
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let host = app_id == HOST;
            let _ = match request {
                DriverRequest::Start(request, responder) => {
                    let ecam = request
                        .device
                        .properties
                        .iter()
                        .find(|p| p.key == "ecam_base");
                    let ecam = ecam.and_then(|p| p.value.strip_prefix("0x"));
                    let ecam = ecam.and_then(|b| u64::from_str_radix(b, 16).ok());
                    let as_expected = host && expected(&request);
                    let bus = request.bus;
                    let client = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
                    let client = Rc::new(client);
                    let result = match app_id.as_str() {
                        HOST if as_expected => Ok(DriverStartResponse {}),
                        CHILD => {
                            let refused =
                                client.request_resource(KIND_MMIO, 0xFE00_0000, 0x1000, false);
                            match matches!(refused.await, Ok(Err(Status::AccessDenied))) {
                                true => signal(bus.as_ref()).map(|()| DriverStartResponse {}),
                                false => Err(Status::InvalidArgs),
                            }
                        }
                        DECLINE => Err(Status::NotSupported),
                        CLOSE => return 0,
                        _ => Err(Status::InvalidArgs),
                    };
                    let started = result.is_ok();
                    let replied = responder.reply(s, result);
                    if host && started {
                        *manager.borrow_mut() = Some(client.clone());
                        spawner.spawn(add_child(client, ecam.unwrap_or(0)));
                    }
                    replied
                }
                DriverRequest::Stop(_, responder) => {
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::ResetChild(_, responder) => match host {
                    true => responder.reply(s, Ok(DriverResetChildResponse {})),
                    false => responder.reply(s, Err(Status::NotSupported)),
                },
                DriverRequest::ConnectChild(request, responder) => {
                    let client = manager.borrow().clone();
                    match (host, client) {
                        (true, Some(client)) => {
                            let replied = responder.reply(s, Ok(DriverConnectChildResponse {}));
                            let (device, bus, dma) = (request.device, request.bus, request.dma);
                            spawner.spawn(remove_child(client, reactor.clone(), device, bus, dma));
                            replied
                        }
                        _ => responder.reply(s, Err(Status::NotSupported)),
                    }
                }
            };
        }
        0
    })
}

/// As the host bridge's driver, with its ECAM range at `ecam`: checks
/// that the device manager refuses the requests it should and grants
/// the others, and adds the child device, named after the outcome.
async fn add_child(manager: Rc<DeviceManagerAsyncClient>, ecam: u64) {
    let bad = manager
        .add_device("bad".into(), vec!["Bad:1".into()], vec![], vec![])
        .await;
    let own = manager.remove_device(1).await;
    let unknown = manager.remove_device(999).await;
    let request = |kind, base, size, shared| manager.request_resource(kind, base, size, shared);
    let granted = |reply: Result<Result<DeviceManagerRequestResourceResponse, Status>, _>,
                   wanted| {
        matches!(reply, Ok(Ok(r)) if kind(&r.resource) == Some(wanted))
    };
    let mmio = granted(request(KIND_MMIO, ecam, 0x1000, true).await, KIND_MMIO);
    let bti = granted(request(KIND_BTI, 0, 0x100, false).await, KIND_BTI);
    let dma = request(KIND_DMA, 0, 0, false).await;
    let other_segment = request(KIND_BTI, 0x1_0000, 1, false).await;
    let checked = matches!(bad, Ok(Err(Status::InvalidArgs)))
        && matches!(own, Ok(Err(Status::AccessDenied)))
        && matches!(unknown, Ok(Err(Status::NotFound)))
        && mmio
        && bti
        && matches!(dma, Ok(Err(Status::InvalidArgs)))
        && matches!(other_segment, Ok(Err(Status::AccessDenied)));
    let name = if checked {
        "test-child"
    } else {
        "checks-failed"
    };
    let slot = Property {
        key: "slot".into(),
        value: "0".into(),
    };
    let _ = manager
        .add_device(name.into(), vec!["test:child".into()], vec![slot], vec![])
        .await;
}

/// As the host bridge's driver: once the driver of the child `device`
/// wrote on its bus channel `bus`, removes the child, when `dma` says
/// that its driver needs `dma`.
async fn remove_child(
    manager: Rc<DeviceManagerAsyncClient>,
    reactor: Reactor,
    device: u64,
    bus: OwnedHandle,
    dma: bool,
) {
    if reactor.signals(bus.raw(), READABLE).await.is_ok() && dma {
        let _ = manager.remove_device(device).await;
    }
    drop(bus);
}

/// As the child's driver: writes a message on the bus channel `bus`.
fn signal(bus: Option<&OwnedHandle>) -> Result<(), Status> {
    let bus = bus.ok_or(Status::InvalidArgs)?;
    mikros_rt::midl::channel_write(bus.raw(), b"started", &[]).map_err(|_| Status::Internal)
}

/// Whether `Start` describes the q35 host bridge with the resources
/// the driver's manifest asks for.
fn expected(request: &DriverStartRequest) -> bool {
    let device = &request.device;
    let property = |key: &str| {
        device
            .properties
            .iter()
            .find(|p| p.key == key)
            .map(|p| p.value.as_str())
    };
    let kinds: alloc::vec::Vec<Option<u32>> = request.resources.iter().map(kind).collect();
    device.name == "pci-host-0"
        && device.hardware_ids == ["platform:pci-host-ecam"]
        && property("segment") == Some("0")
        && property("bus_start") == Some("0")
        && property("bus_end") == Some("255")
        && property("ecam_base").is_some_and(|b| b.starts_with("0x"))
        && request.bus.is_none()
        && kinds == [Some(KIND_MMIO), Some(KIND_DMA)]
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
