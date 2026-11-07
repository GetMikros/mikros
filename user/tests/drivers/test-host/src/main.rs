//! A driver for PCI host bridges, for the device manager tests.
//!
//! It serves `Driver` on its `PROVIDER` startup handle. `Start` succeeds
//! when the driver runs as `system.driver.test-host` and is started for
//! the q35 host bridge, `pci-host-0`, as `platform:pci-host-ecam`, with
//! its properties, no bus channel, and an `MMIO` and a `DMA` resource in
//! that order; it fails with `INVALID_ARGS` otherwise. `Stop` ends the
//! driver, and `ResetChild` fails with `NOT_SUPPORTED`, as it adds no
//! devices.

#![no_std]
#![no_main]

extern crate alloc;

use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DriverAsyncServer, DriverRequest, DriverStartRequest, DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const APP_ID: &str = "system.driver.test-host";
const KIND_MMIO: u32 = 1;
const KIND_DMA: u32 = 4;
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let as_driver = managed.header.app_id == APP_ID;
    let Some(at) = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::Provider)
    else {
        return PANIC_EXIT_CODE;
    };
    let end = ServerEnd::new(managed.handles.remove(at).handle);
    let server = DriverAsyncServer::new(end, reactor);
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(request, responder) => {
                    let result = match as_driver && expected(&request) {
                        true => Ok(DriverStartResponse {}),
                        false => Err(Status::InvalidArgs),
                    };
                    responder.reply(s, result)
                }
                DriverRequest::Stop(_, responder) => {
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::ResetChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
            };
        }
        0
    })
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
