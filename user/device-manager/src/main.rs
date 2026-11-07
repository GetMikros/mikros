//! The device manager.
//!
//! It keeps the tree of devices. At startup it adds the root devices of
//! the platform: `platform:boot-framebuffer` when the service manager
//! gave it the framebuffer, and `platform:pci-host-ecam` for each entry
//! of the ACPI MCFG table, which it finds from the RSDP that the
//! platform information names. Each host bridge has the properties
//! `segment`, `bus_start` and `bus_end` in decimal and `ecam_base` in
//! hexadecimal with `0x`, and a shared `MMIO` resource covering the
//! configuration space of its buses.
//!
//! It then reads the driver manifests, binds a driver to each device
//! that one matches, and starts and supervises the bound drivers.

#![no_std]
#![no_main]

extern crate alloc;

mod acpi;
mod devices;
mod drivers;
mod start;
mod supervise;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use devices::Devices;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::fs::NodeAsyncClient;
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient};
use mikros_rt::interfaces::mikros::process::LauncherAsyncClient;
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use start::Starter;
use supervise::Manager;

mikros_rt::raw_main!(main);

/// The `id`s of the boot handles among the `USER` startup handles.
const ROOT_RESOURCE_ID: u32 = 1;
const LOG_ID: u32 = 2;
const FRAMEBUFFER_ID: u32 = 4;
const PLATFORM_INFO_ID: u32 = 5;
/// The platform information's header: its size, the offset of the
/// physical address of the firmware's tables, and that of its flags,
/// with the bit set when a framebuffer is present.
const INFO_HEADER_SIZE: usize = 0x68;
const FIRMWARE_TABLE: usize = 0x08;
const FLAGS: usize = 0x04;
const HAS_FRAMEBUFFER: u32 = 1 << 0;
const KIND_MMIO: u32 = 1;
const SHARED: u32 = 1 << 0;
/// The configuration space of one bus, in an ECAM range.
const BUS_SIZE: u64 = 1 << 20;
/// The permission broker's protocol.
const BROKER: &str = "mikros.permissions.Broker";

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
    let log = take(&mut managed.handles, LOG_ID);
    if let Some(log) = &log {
        mikros_rt::log::set_handle(log.raw());
    }
    let Some(root) = take(&mut managed.handles, ROOT_RESOURCE_ID) else {
        mikros_rt::log!(Error, "device-manager: no root resource");
        return PANIC_EXIT_CODE;
    };
    let framebuffer = take(&mut managed.handles, FRAMEBUFFER_ID);
    let info = take(&mut managed.handles, PLATFORM_INFO_ID)
        .as_ref()
        .and_then(platform_info);
    let mut devices = Devices::new();
    let has_framebuffer = info
        .as_ref()
        .is_some_and(|i| u32_at(i, FLAGS) & HAS_FRAMEBUFFER != 0);
    if framebuffer.is_some() && has_framebuffer {
        devices.add(
            "boot-framebuffer".into(),
            vec!["platform:boot-framebuffer".into()],
            Vec::new(),
            None,
            Vec::new(),
        );
    }
    let memory = acpi::Memory {
        root: &root,
        process: managed.header.process.raw(),
    };
    let rsdp = info.map(|i| u64_at(&i, FIRMWARE_TABLE)).filter(|&r| r != 0);
    match rsdp.and_then(|rsdp| memory.find(rsdp, b"MCFG")) {
        Some(mcfg) => add_host_bridges(&mut devices, &root, &mcfg),
        None => mikros_rt::log!(Warning, "device-manager: no MCFG table, so no PCI"),
    }
    let root_directory = managed
        .namespace
        .iter()
        .position(|e| e.path == "/")
        .map(|at| managed.namespace.swap_remove(at).directory);
    let drivers = match root_directory {
        Some(directory) => {
            let directory = NodeAsyncClient::new(directory, reactor.clone());
            executor.block_on(drivers::read_all(&directory, &reactor))
        }
        None => {
            mikros_rt::log!(Error, "device-manager: the namespace has no root");
            Vec::new()
        }
    };
    devices.bind(&drivers);
    let header = managed.header;
    let directory = DirectoryAsyncClient::new(header.directory, reactor.clone());
    let broker = executor.block_on(connect_broker(&directory, &reactor));
    let launcher = LauncherAsyncClient::new(header.launcher, reactor.clone());
    let starter = Starter::new(launcher, broker, root, reactor.clone());
    let manager = Rc::new(Manager {
        devices: RefCell::new(devices),
        drivers,
        starter,
        reactor,
    });
    let bound = manager.devices.borrow().bound();
    for id in bound {
        spawner.spawn(supervise::supervise(manager.clone(), id));
    }
    // The handles, the framebuffer's and the process's among them, stay
    // open.
    executor.block_on(core::future::pending::<()>());
    drop((log, framebuffer, header.process, directory, manager));
    PANIC_EXIT_CODE
}

/// A connection to the permission broker through `directory`, or none
/// when the directory refuses it, which is logged.
async fn connect_broker(
    directory: &DirectoryAsyncClient,
    reactor: &mikros_rt::exec::Reactor,
) -> Option<BrokerAsyncClient> {
    let (client, server) = endpoints::<Broker>().ok()?;
    match directory
        .connect(BROKER.into(), None, server.into_handle())
        .await
    {
        Ok(Ok(_)) => Some(BrokerAsyncClient::new(client, reactor.clone())),
        Ok(Err(status)) => {
            mikros_rt::log!(
                Error,
                "device-manager: connecting to the broker: {status:?}"
            );
            None
        }
        Err(e) => {
            mikros_rt::log!(Error, "device-manager: connecting to the broker: {e:?}");
            None
        }
    }
}

/// Adds a `platform:pci-host-ecam` root device for each entry of `mcfg`,
/// with a shared `MMIO` resource from `root` for its buses' range.
fn add_host_bridges(devices: &mut Devices, root: &OwnedHandle, mcfg: &[u8]) {
    for ecam in acpi::ecams(mcfg) {
        let buses = u64::from(ecam.bus_end - ecam.bus_start) + 1;
        let base = ecam.base + u64::from(ecam.bus_start) * BUS_SIZE;
        let mut resource = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe {
            sys::resource_create(
                root.raw(),
                KIND_MMIO,
                base,
                buses * BUS_SIZE,
                SHARED,
                &mut resource,
            )
        };
        if status != mikros_rt::OK {
            mikros_rt::log!(
                Error,
                "device-manager: no resource for ECAM at {base:#x}: {status}"
            );
            continue;
        }
        // SAFETY: the kernel just created the handle for us.
        let resource = unsafe { OwnedHandle::from_raw(resource) };
        let properties = vec![
            property("segment", format!("{}", ecam.segment)),
            property("bus_start", format!("{}", ecam.bus_start)),
            property("bus_end", format!("{}", ecam.bus_end)),
            property("ecam_base", format!("{:#x}", ecam.base)),
        ];
        devices.add(
            format!("pci-host-{}", ecam.segment),
            vec!["platform:pci-host-ecam".into()],
            properties,
            None,
            vec![resource],
        );
    }
}

fn property(key: &str, value: String) -> (String, String) {
    (key.into(), value)
}

/// The header of the platform information in `mem`, if it holds one.
fn platform_info(mem: &OwnedHandle) -> Option<[u8; INFO_HEADER_SIZE]> {
    let mut header = [0u8; INFO_HEADER_SIZE];
    // SAFETY: the pointer describes `header`.
    let status =
        unsafe { sys::mem_read(mem.raw(), 0, header.as_mut_ptr(), INFO_HEADER_SIZE as u64) };
    (status == mikros_rt::OK && u32_at(&header, 0) == 1).then_some(header)
}

/// Removes the `USER` startup handle with `id` from `handles`.
fn take(handles: &mut Vec<StartupHandle>, id: u32) -> Option<OwnedHandle> {
    let at = handles
        .iter()
        .position(|h| h.role == HandleRole::User && h.id == id)?;
    Some(handles.remove(at).handle)
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}
