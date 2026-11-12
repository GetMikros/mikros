//! The device manager.
//!
//! It keeps the tree of devices. At startup it adds the root devices of
//! the platform: `platform:boot-framebuffer` when the service manager
//! gave it the framebuffer, and `platform:pci-host-ecam` for each entry
//! of the ACPI MCFG table, which it finds from the RSDP that the
//! platform information names. Each host bridge has the properties
//! `segment`, `bus_start` and `bus_end` in decimal and `ecam_base` in
//! hexadecimal with `0x`, and a shared `MMIO` resource covering the
//! configuration space of its buses. After the host bridges, when the
//! platform information names an RSDP, it adds `acpi` as
//! `platform:acpi`, with the property `rsdp`, the RSDP's physical address
//! in hexadecimal with `0x`, and no resources.
//!
//! It then reads the driver manifests, binds a driver to each device
//! that one matches, and starts and supervises the bound drivers.

#![no_std]
#![no_main]

extern crate alloc;

mod acpi;
mod boot_display;
mod cache;
mod devices;
mod drivers;
mod publish;
mod removal;
mod serve;
mod shutdown;
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
use mikros_rt::interfaces::__midl::{ClientEnd, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::fs::NodeAsyncClient;
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient};
use mikros_rt::interfaces::mikros::process::LauncherAsyncClient;
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::interfaces::mikros::service::{
    DirectoryAsyncClient, Registrar, RegistrarAsyncClient,
};
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
/// The flag set when the header names the framebuffer's PCI function,
/// and where it does, as segment, bus and devfn in one word.
const HAS_FRAMEBUFFER_PCI: u32 = 1 << 2;
const FRAMEBUFFER_PCI: usize = 0x5C;
/// Where the header holds the framebuffer's description.
const FRAMEBUFFER: usize = 0x10;
/// Where the header gives the command line's offset and length, and the
/// longest command line.
const CMDLINE_OFFSET: usize = 0x48;
const CMDLINE_LEN: usize = 0x4C;
const CMDLINE_MAX: usize = 4096;
/// Where the header gives the memory map's offset and entry count, the
/// size of an entry, the offsets of an entry's start, page count and
/// kind, and the kinds `AcpiNvs` and `Reserved`.
const MEMORY_MAP_OFFSET: usize = 0x50;
const MEMORY_MAP_COUNT: usize = 0x54;
const MAP_ENTRY_SIZE: usize = 24;
const ENTRY_PAGES: usize = 0x08;
const ENTRY_KIND: usize = 0x10;
const KIND_ACPI_NVS: u32 = 8;
const KIND_RESERVED: u32 = 13;
const PAGE_SIZE: u64 = 4096;
const KIND_MMIO: u32 = 1;
const SHARED: u32 = 1 << 0;
/// The configuration space of one bus, in an ECAM range.
const BUS_SIZE: u64 = 1 << 20;
/// The protocols of the permission broker and the service manager's
/// registrar.
const BROKER: &str = "mikros.permissions.Broker";
const REGISTRAR: &str = "mikros.service.Registrar";

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    // A `TERM` that arrives before the devices are known stays pending
    // until `shutdown::watch` takes it.
    shutdown::block_term();
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
    let provider = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::Provider)
        .map(|at| managed.handles.remove(at).handle);
    let info_memory = take(&mut managed.handles, PLATFORM_INFO_ID);
    let info = info_memory.as_ref().and_then(platform_info);
    let command_line = match (&info_memory, &info) {
        (Some(memory), Some(header)) => command_line(memory, header),
        _ => String::new(),
    };
    let firmware_memory = match (&info_memory, &info) {
        (Some(memory), Some(header)) => firmware_memory(memory, header),
        _ => Vec::new(),
    };
    let mut devices = Devices::new();
    let has_framebuffer = info
        .as_ref()
        .is_some_and(|i| u32_at(i, FLAGS) & HAS_FRAMEBUFFER != 0);
    let memory = acpi::Memory {
        root: &root,
        process: managed.header.process.raw(),
    };
    let rsdp = info.map(|i| u64_at(&i, FIRMWARE_TABLE)).filter(|&r| r != 0);
    let mcfg = rsdp.and_then(|rsdp| memory.find(rsdp, b"MCFG"));
    // The framebuffer's PCI function, when the header names one and an
    // MCFG entry covers it.
    let pci = info
        .as_ref()
        .filter(|i| u32_at(&i[..], FLAGS) & HAS_FRAMEBUFFER_PCI != 0)
        .map(|i| u32_at(&i[..], FRAMEBUFFER_PCI))
        .filter(|&pci| {
            mcfg.as_ref().is_some_and(|mcfg| {
                acpi::ecams(mcfg)
                    .any(|e| boot_display::covers(e.segment, e.bus_start, e.bus_end, pci))
            })
        });
    let framebuffer_header = info
        .as_ref()
        .filter(|_| framebuffer.is_some() && has_framebuffer);
    if pci.is_none()
        && let Some(header) = framebuffer_header
    {
        add_boot_framebuffer(&mut devices, header);
    }
    let hosts = match &mcfg {
        Some(mcfg) => add_host_bridges(&mut devices, &root, mcfg),
        None => {
            mikros_rt::log!(Warning, "device-manager: no MCFG table, so no PCI");
            Vec::new()
        }
    };
    let mut boot_display = None;
    if let (Some(pci), Some(header)) = (pci, framebuffer_header) {
        let host = hosts
            .iter()
            .find(|(segment, start, end, _)| boot_display::covers(*segment, *start, *end, pci));
        if let Some(&(_, _, _, host)) = host {
            boot_display = Some(boot_display::BootDisplay {
                name: boot_display::function_name(pci),
                properties: framebuffer_properties(header),
                host,
                given: core::cell::Cell::new(boot_display::Given::Waiting),
            });
        }
    }
    // After the host bridges, so that they keep their IDs.
    if let Some(rsdp) = rsdp {
        devices.add(
            "acpi".into(),
            vec!["platform:acpi".into()],
            vec![("rsdp".into(), format!("{rsdp:#x}"))],
            None,
            Vec::new(),
        );
    }
    let drivers = match managed::take_entry(&mut managed.namespace, drivers::DRIVERS) {
        Some((directory, at)) => {
            let directory = NodeAsyncClient::new(directory, reactor.clone());
            executor.block_on(drivers::read_all(&directory, at, &reactor))
        }
        None => {
            mikros_rt::log!(
                Error,
                "device-manager: the namespace has no {}",
                drivers::DRIVERS
            );
            Vec::new()
        }
    };
    devices.bind(&drivers);
    // Without a bus driver for its host bridge, the framebuffer's PCI
    // function is never added, so the framebuffer gets a device of its own.
    if let Some(boot) = &boot_display
        && devices
            .get(boot.host)
            .is_none_or(|host| host.driver.is_none())
        && let Some(header) = framebuffer_header
    {
        boot.given.set(boot_display::Given::Platform);
        let id = add_boot_framebuffer(&mut devices, header);
        devices.bind_device(id, &drivers);
    }
    let header = managed.header;
    let directory = DirectoryAsyncClient::new(header.directory, reactor.clone());
    let broker = executor.block_on(connect::<Broker>(&directory, BROKER));
    let broker = broker.map(|b| BrokerAsyncClient::new(b, reactor.clone()));
    let registrar = executor.block_on(connect::<Registrar>(&directory, REGISTRAR));
    let registrar = registrar.map(|r| RegistrarAsyncClient::new(r, reactor.clone()));
    let launcher = LauncherAsyncClient::new(header.launcher, reactor.clone());
    let console = log.as_ref().and_then(|l| start::duplicate(l).ok());
    let shared = framebuffer.as_ref().and_then(|f| start::duplicate(f).ok());
    let starter = Starter::new(launcher, broker, root, shared, console, reactor.clone());
    let manager = Rc::new(Manager {
        devices: RefCell::new(devices),
        drivers,
        starter,
        registrar,
        service_ids: publish::ServiceIds::new(),
        directory,
        profiles: cache::Profiles::new(&command_line),
        reactor,
        spawner: spawner.clone(),
        firmware_memory,
        power_off: core::cell::Cell::new(false),
        boot_display,
    });
    spawner.spawn(shutdown::watch(manager.clone()));
    match provider {
        Some(end) => spawner.spawn(removal::serve(manager.clone(), ServerEnd::new(end))),
        None => mikros_rt::log!(
            Warning,
            "device-manager: no provider, so no removal service"
        ),
    }
    let bound = manager.devices.borrow().bound();
    for id in bound {
        spawner.spawn(supervise::supervise(manager.clone(), id));
    }
    // The handles, the framebuffer's and the process's among them, stay
    // open.
    executor.block_on(core::future::pending::<()>());
    drop((log, framebuffer, header.process, manager));
    PANIC_EXIT_CODE
}

/// A connection to `protocol` through `directory`, or none when the
/// directory refuses it, which is logged.
async fn connect<P>(directory: &DirectoryAsyncClient, protocol: &str) -> Option<ClientEnd<P>> {
    let (client, server) = endpoints::<P>().ok()?;
    match directory
        .connect(protocol.into(), None, server.into_handle())
        .await
    {
        Ok(Ok(_)) => Some(client),
        Ok(Err(status)) => {
            mikros_rt::log!(
                Error,
                "device-manager: connecting to {protocol}: {status:?}"
            );
            None
        }
        Err(e) => {
            mikros_rt::log!(Error, "device-manager: connecting to {protocol}: {e:?}");
            None
        }
    }
}

/// Adds a `platform:pci-host-ecam` root device for each entry of `mcfg`,
/// with a shared `MMIO` resource from `root` for its buses' range, and
/// returns the segment, first and last bus, and device ID of each.
fn add_host_bridges(
    devices: &mut Devices,
    root: &OwnedHandle,
    mcfg: &[u8],
) -> Vec<(u16, u8, u8, u64)> {
    let mut added = Vec::new();
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
        let id = devices.add(
            format!("pci-host-{}", ecam.segment),
            vec!["platform:pci-host-ecam".into()],
            properties,
            None,
            vec![resource],
        );
        added.push((ecam.segment, ecam.bus_start, ecam.bus_end, id));
    }
    added
}

/// Adds the root device that has the boot framebuffer, described by the
/// platform information's `header`, and returns its ID.
fn add_boot_framebuffer(devices: &mut Devices, header: &[u8]) -> u64 {
    devices.add(
        "boot-framebuffer".into(),
        vec![boot_display::PLATFORM_BOOT_FRAMEBUFFER.into()],
        framebuffer_properties(header),
        None,
        Vec::new(),
    )
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

/// The command line that the platform information `mem` with the header
/// `header` holds, or an empty one when it cannot be read or is not
/// UTF-8.
fn command_line(mem: &OwnedHandle, header: &[u8; INFO_HEADER_SIZE]) -> String {
    let offset = u32_at(header, CMDLINE_OFFSET) as u64;
    let len = (u32_at(header, CMDLINE_LEN) as usize).min(CMDLINE_MAX);
    let mut bytes = vec![0u8; len];
    // SAFETY: the pointer describes `bytes`.
    let status = unsafe { sys::mem_read(mem.raw(), offset, bytes.as_mut_ptr(), len as u64) };
    if status != mikros_rt::OK {
        return String::new();
    }
    String::from_utf8(bytes).unwrap_or_default()
}

/// The ranges of the memory map in the platform information `mem` with
/// the header `header` whose kind is `AcpiNvs` or `Reserved`, as start
/// and size, in ascending order, with ranges that touch merged. Empty
/// when the map cannot be read.
fn firmware_memory(mem: &OwnedHandle, header: &[u8; INFO_HEADER_SIZE]) -> Vec<(u64, u64)> {
    let offset = u64::from(u32_at(header, MEMORY_MAP_OFFSET));
    let count = u32_at(header, MEMORY_MAP_COUNT) as usize;
    let mut entries = vec![0u8; count * MAP_ENTRY_SIZE];
    // SAFETY: the pointer describes `entries`.
    let status = unsafe {
        sys::mem_read(
            mem.raw(),
            offset,
            entries.as_mut_ptr(),
            entries.len() as u64,
        )
    };
    if status != mikros_rt::OK {
        return Vec::new();
    }
    let (entries, _) = entries.as_chunks::<MAP_ENTRY_SIZE>();
    let mut ranges: Vec<(u64, u64)> = entries
        .iter()
        .filter(|e| matches!(u32_at(*e, ENTRY_KIND), KIND_ACPI_NVS | KIND_RESERVED))
        .filter_map(|e| {
            let size = u64_at(e, ENTRY_PAGES).checked_mul(PAGE_SIZE)?;
            (size != 0).then_some((u64_at(e, 0), size))
        })
        .collect();
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (start, size) in ranges {
        match merged.last_mut() {
            Some((last, len)) if *last + *len == start => *len += size,
            _ => merged.push((start, size)),
        }
    }
    merged
}

/// Removes the `USER` startup handle with `id` from `handles`.
fn take(handles: &mut Vec<StartupHandle>, id: u32) -> Option<OwnedHandle> {
    let at = handles
        .iter()
        .position(|h| h.role == HandleRole::User && h.id == id)?;
    Some(handles.remove(at).handle)
}

/// The properties of the boot framebuffer's device: its description from
/// the platform information's header, the sizes and format in decimal
/// and the masks as `0x` and lowercase hexadecimal digits.
fn framebuffer_properties(header: &[u8]) -> Vec<(String, String)> {
    let decimal = [
        ("size", u64_at(header, FRAMEBUFFER + 0x08)),
        ("width", u64::from(u32_at(header, FRAMEBUFFER + 0x10))),
        ("height", u64::from(u32_at(header, FRAMEBUFFER + 0x14))),
        ("stride", u64::from(u32_at(header, FRAMEBUFFER + 0x18))),
        ("format", u64::from(u32_at(header, FRAMEBUFFER + 0x1C))),
    ];
    let masks = [
        ("red_mask", 0x20),
        ("green_mask", 0x24),
        ("blue_mask", 0x28),
        ("reserved_mask", 0x2C),
    ];
    let mut properties: Vec<(String, String)> = decimal
        .iter()
        .map(|(k, v)| ((*k).into(), format!("{v}")))
        .collect();
    properties.extend(masks.iter().map(|(k, at)| {
        (
            (*k).into(),
            format!("{:#x}", u32_at(header, FRAMEBUFFER + at)),
        )
    }));
    properties
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}
