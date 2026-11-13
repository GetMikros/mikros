//! The interrupts of the functions: INTx lines, which the bus driver
//! holds and shares between the functions on them, and message-signaled
//! interrupts.

use crate::driver::bus::{Bus, Function};
use crate::pci::{Address, CAP_MSI, CAP_MSIX, COMMAND, Config, STATUS, capabilities};
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::acpi::{Acpi, AcpiAsyncClient};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;

pub const KIND_IRQ: u32 = 3;
pub const KIND_MSI: u32 = 6;
/// The service of the ACPI server, which routes INTx pins.
const ACPI: &str = "device.acpi";
/// The `Interrupt Line` register, and the value that means none.
const INTERRUPT_LINE: u16 = 0x3C;
const NO_LINE: u8 = 0xFF;
/// The command register's bit that disables INTx, and the status
/// register's bit of a pending INTx.
const INTERRUPT_DISABLE: u32 = 1 << 10;
const INTERRUPT_STATUS: u32 = 1 << 3;
/// `interrupt_create` options.
const LEVEL: u32 = 1 << 0;
const ACTIVE_LOW: u32 = 1 << 1;
const SIGNALED: u32 = 1 << 3;
/// Handle rights.
const DUPLICATE: u32 = 1 << 0;
const TRANSFER: u32 = 1 << 1;
const WRITE: u32 = 1 << 3;
const WAIT: u32 = 1 << 6;
const INSPECT: u32 = 1 << 9;
/// MSI and MSI-X registers: message control's enable bits, the 64-bit
/// address bit of MSI, the MSI-X function mask and table size, and the
/// table's BAR indicator.
const MSI_ENABLE: u32 = 1 << 0;
const MSI_64: u32 = 1 << 7;
const MSIX_ENABLE: u32 = 1 << 15;
const MSIX_MASK: u32 = 1 << 14;
const MSIX_TABLE_SIZE: u32 = 0x7FF;
const MSIX_BIR: u32 = 0x7;
/// The most message-signaled interrupts that one call creates.
const MSI_MAX: u32 = 32;
const UNCACHED_DEVICE: u32 = 2;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const PAGE_SIZE: u64 = 4096;

/// A function's INTx: the global system interrupt of its line, and the
/// event that the line signals for it.
pub struct Intx {
    pub gsi: u32,
    pub event: OwnedHandle,
}

/// An INTx line that the bus driver holds: its `IRQ` resource, its
/// `Interrupt` object, and the functions on it.
pub struct Line {
    _resource: OwnedHandle,
    interrupt: OwnedHandle,
    functions: core::cell::RefCell<Vec<Rc<Function>>>,
}

/// How an INTx pin reaches the interrupt controller.
struct Route {
    gsi: u32,
    level: bool,
    active_low: bool,
}

/// The event of the INTx of `function`, with `WAIT` and without
/// `SIGNAL`. On each call the line is found anew: from the ACPI server
/// when the directory lists one, and from the `Interrupt Line` register,
/// level-triggered and active low, otherwise. Fails with `NOT_SUPPORTED`
/// for a function without INTx.
pub async fn get_interrupt(bus: &Rc<Bus>, function: &Rc<Function>) -> Result<OwnedHandle, Status> {
    let route = route(bus, function).await?;
    let current = function.intx.borrow().as_ref().map(|i| i.gsi);
    if current != Some(route.gsi) {
        if let Some(old) = current {
            leave(bus, function, old);
        }
        let line = line(bus, &route).await?;
        let event = event_create()?;
        *function.intx.borrow_mut() = Some(Intx {
            gsi: route.gsi,
            event,
        });
        line.functions.borrow_mut().push(function.clone());
    }
    let intx = function.intx.borrow();
    let event = &intx.as_ref().ok_or(Status::BadState)?.event;
    duplicate(event, DUPLICATE | TRANSFER | WAIT | INSPECT)
}

/// Clears the INTx event of `function` and lets it raise INTx again.
pub fn ack_interrupt(bus: &Bus, function: &Function) -> Result<(), Status> {
    let intx = function.intx.borrow();
    let intx = intx.as_ref().ok_or(Status::BadState)?;
    // SAFETY: the call takes no pointers.
    unsafe { sys::object_signal(intx.event.raw(), SIGNALED, 0) };
    let mut config = &bus.ecam;
    let command = config.read(function.at, COMMAND, 2);
    config.write(function.at, COMMAND, 2, command & !INTERRUPT_DISABLE);
    Ok(())
}

/// The route of the INTx pin of `function`.
async fn route(bus: &Bus, function: &Function) -> Result<Route, Status> {
    let pin = function.header.interrupt_pin;
    if pin == 0 {
        return Err(Status::NotSupported);
    }
    let listed = bus.directory.list(ACPI.into()).await;
    let instance = match listed {
        Ok(Ok(listed)) => listed.instances.into_iter().next(),
        _ => None,
    };
    if let Some(instance) = instance {
        let (client, server) = endpoints::<Acpi>().map_err(|_| Status::NoResources)?;
        let connected = bus
            .directory
            .connect(ACPI.into(), Some(instance), server.into_handle())
            .await;
        match connected {
            Ok(Ok(_)) => {}
            Ok(Err(status)) => return Err(status),
            Err(_) => return Err(Status::PeerClosed),
        }
        let acpi = AcpiAsyncClient::new(client, bus.reactor.clone());
        let at = function.at;
        return match acpi
            .get_pci_route(bus.segment, at.bus, at.device, pin)
            .await
        {
            Ok(Ok(r)) => Ok(Route {
                gsi: r.route.gsi,
                level: r.route.level,
                active_low: r.route.active_low,
            }),
            Ok(Err(status)) => Err(status),
            Err(_) => Err(Status::PeerClosed),
        };
    }
    let line = (&bus.ecam).read(function.at, INTERRUPT_LINE, 1) as u8;
    if line == NO_LINE {
        return Err(Status::NotSupported);
    }
    Ok(Route {
        gsi: u32::from(line),
        level: true,
        active_low: true,
    })
}

/// The line of `route`, which the bus driver takes, from a shared `IRQ`
/// resource, so that other holders such as the ACPI server can have it
/// too, and watches, the first time a function is on it.
async fn line(bus: &Rc<Bus>, route: &Route) -> Result<Rc<Line>, Status> {
    if let Some(line) = bus.lines.borrow().get(&route.gsi) {
        return Ok(line.clone());
    }
    let granted = bus
        .manager
        .request_resource(KIND_IRQ, u64::from(route.gsi), 1, true)
        .await;
    let resource = match granted {
        Ok(Ok(granted)) => granted.resource,
        Ok(Err(status)) => return Err(status),
        Err(_) => return Err(Status::PeerClosed),
    };
    // Another function may have taken the line meanwhile.
    if let Some(line) = bus.lines.borrow().get(&route.gsi) {
        return Ok(line.clone());
    }
    let options =
        if route.level { LEVEL } else { 0 } | if route.active_low { ACTIVE_LOW } else { 0 };
    let mut interrupt = 0;
    // SAFETY: the out pointer is valid for the call.
    let status =
        unsafe { sys::interrupt_create(resource.raw(), route.gsi, options, &mut interrupt) };
    if status != mikros_rt::OK {
        return Err(Status::from_raw(status as i32));
    }
    let line = Rc::new(Line {
        _resource: resource,
        // SAFETY: the kernel just created the handle for us.
        interrupt: unsafe { OwnedHandle::from_raw(interrupt) },
        functions: core::cell::RefCell::new(Vec::new()),
    });
    bus.lines.borrow_mut().insert(route.gsi, line.clone());
    bus.spawner.spawn(watch(bus.clone(), line.clone()));
    Ok(line)
}

/// Takes `function` off the line of `gsi`.
fn leave(bus: &Bus, function: &Function, gsi: u32) {
    if let Some(line) = bus.lines.borrow().get(&gsi) {
        line.functions.borrow_mut().retain(|f| f.at != function.at);
    }
}

/// Each time `line` is raised: for every function on it whose
/// `Interrupt Status` bit is set, disables its INTx and signals its
/// event, and then acknowledges the line.
async fn watch(bus: Rc<Bus>, line: Rc<Line>) {
    while bus
        .reactor
        .signals(line.interrupt.raw(), SIGNALED)
        .await
        .is_ok()
    {
        let mut config = &bus.ecam;
        for function in line.functions.borrow().iter() {
            let at = function.at;
            if config.read(at, STATUS, 2) & INTERRUPT_STATUS == 0 {
                continue;
            }
            let command = config.read(at, COMMAND, 2);
            config.write(at, COMMAND, 2, command | INTERRUPT_DISABLE);
            if let Some(intx) = function.intx.borrow().as_ref() {
                // SAFETY: the call takes no pointers.
                unsafe { sys::object_signal(intx.event.raw(), 0, SIGNALED) };
            }
        }
        // SAFETY: the call takes no pointers.
        unsafe { sys::interrupt_ack(line.interrupt.raw()) };
    }
}

/// The message-signaled interrupts of one connection: the capability
/// that delivers them, their objects, and the MSI-X table's mapping.
pub struct Msi {
    at: Address,
    capability: (u8, u16),
    _interrupts: Vec<OwnedHandle>,
    table: Option<(u64, u64)>,
}

/// Enables up to `count` message-signaled interrupts for `function`:
/// through its MSI-X capability, at most its table size, or else its
/// MSI capability, with one message. Returns the state that `disable`
/// takes and the interrupts, with `WAIT`, `WRITE` and `INSPECT`. Fails
/// with `NOT_SUPPORTED` when the function has neither capability.
pub async fn enable_msi(
    bus: &Bus,
    function: &Function,
    process: sys::Handle,
    count: u32,
) -> Result<(Msi, Vec<OwnedHandle>), Status> {
    let at = function.at;
    let mut config = &bus.ecam;
    let caps = capabilities(&config, at);
    let msix = caps.iter().find(|(id, _)| *id == CAP_MSIX).copied();
    let msi = caps.iter().find(|(id, _)| *id == CAP_MSI).copied();
    let capability = msix.or(msi).ok_or(Status::NotSupported)?;
    if count == 0 {
        return Err(Status::InvalidArgs);
    }
    let (_, cap) = capability;
    let control = config.read(at, cap + 2, 2);
    let n = match msix {
        Some(_) => count.min((control & MSIX_TABLE_SIZE) + 1).min(MSI_MAX),
        None => 1,
    };
    let resource = msi_resource(bus).await?;
    let bti = function.bti.as_ref().map_err(|s| *s)?;
    let mut handles = vec![0; n as usize];
    let mut messages = vec![0u8; 16 * n as usize];
    // SAFETY: the pointers describe `n` handles and `n` messages.
    let status = unsafe {
        sys::interrupt_create_msi(
            resource.raw(),
            bti.raw(),
            n,
            0,
            handles.as_mut_ptr(),
            messages.as_mut_ptr(),
        )
    };
    if status != mikros_rt::OK {
        return Err(Status::from_raw(status as i32));
    }
    // SAFETY: the kernel just created the handles for us.
    let interrupts: Vec<OwnedHandle> = handles
        .into_iter()
        .map(|h| unsafe { OwnedHandle::from_raw(h) })
        .collect();
    let message = |i: usize| {
        let m = &messages[16 * i..16 * i + 16];
        let address = u64::from_le_bytes(m[..8].try_into().expect("8 bytes"));
        let data = u32::from_le_bytes(m[8..12].try_into().expect("4 bytes"));
        (address, data)
    };
    let mut table = None;
    if msix.is_some() {
        let mapped = map_table(function, process, config.read(at, cap + 4, 4), n)?;
        for i in 0..n as usize {
            let (address, data) = message(i);
            let entry = mapped.0 + 16 * i as u64;
            // SAFETY: the entry lies in the mapped table.
            unsafe {
                core::ptr::write_volatile(entry as *mut u32, address as u32);
                core::ptr::write_volatile((entry + 4) as *mut u32, (address >> 32) as u32);
                core::ptr::write_volatile((entry + 8) as *mut u32, data);
                core::ptr::write_volatile((entry + 12) as *mut u32, 0);
            }
        }
        table = Some((mapped.1, mapped.2));
        config.write(at, cap + 2, 2, (control | MSIX_ENABLE) & !MSIX_MASK);
    } else {
        let (address, data) = message(0);
        config.write(at, cap + 4, 4, address as u32);
        let data_at = if control & MSI_64 != 0 {
            config.write(at, cap + 8, 4, (address >> 32) as u32);
            cap + 12
        } else {
            cap + 8
        };
        config.write(at, data_at, 2, data & 0xFFFF);
        // One message: the multiple message enable field stays 0.
        config.write(at, cap + 2, 2, (control & !(0b111 << 4)) | MSI_ENABLE);
    }
    let command = config.read(at, COMMAND, 2);
    config.write(at, COMMAND, 2, command | INTERRUPT_DISABLE);
    let mut given = Vec::new();
    for interrupt in &interrupts {
        given.push(duplicate(interrupt, TRANSFER | WAIT | WRITE | INSPECT)?);
    }
    let state = Msi {
        at,
        capability,
        _interrupts: interrupts,
        table,
    };
    Ok((state, given))
}

/// Disables the capability of `msi`, lets the function raise INTx again,
/// unmaps the MSI-X table and closes the bus driver's handles to the
/// interrupts.
pub fn disable(bus: &Bus, process: sys::Handle, msi: Msi) {
    let mut config = &bus.ecam;
    let (id, cap) = msi.capability;
    let control = config.read(msi.at, cap + 2, 2);
    let enable = if id == CAP_MSIX {
        MSIX_ENABLE
    } else {
        MSI_ENABLE
    };
    config.write(msi.at, cap + 2, 2, control & !enable);
    let command = config.read(msi.at, COMMAND, 2);
    config.write(msi.at, COMMAND, 2, command & !INTERRUPT_DISABLE);
    if let Some((mapped, size)) = msi.table {
        // SAFETY: nothing refers to the mapping any more.
        unsafe { sys::vm_unmap(process, mapped, size) };
    }
}

/// Maps the first `n` entries of the MSI-X table that `table` places
/// in a BAR of `function`. Returns the address of the first entry, and
/// the mapping's start and size.
fn map_table(
    function: &Function,
    process: sys::Handle,
    table: u32,
    n: u32,
) -> Result<(u64, u64, u64), Status> {
    let index = (table & MSIX_BIR) as u8;
    let offset = u64::from(table & !MSIX_BIR);
    let (bar, resource) = function
        .bars
        .iter()
        .find(|(bar, _)| bar.index == index)
        .ok_or(Status::NotSupported)?;
    let resource = resource.as_ref().map_err(|s| *s)?;
    let start = (bar.base + offset) & !(PAGE_SIZE - 1);
    let end = (bar.base + offset + 16 * u64::from(n)).next_multiple_of(PAGE_SIZE);
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe {
        sys::mem_create_physical(
            resource.raw(),
            start,
            end - start,
            UNCACHED_DEVICE,
            &mut mem,
        )
    };
    if status != mikros_rt::OK {
        return Err(Status::from_raw(status as i32));
    }
    // SAFETY: the kernel just created the handle for us.
    let mem = unsafe { OwnedHandle::from_raw(mem) };
    let mut mapped = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let status = unsafe {
        sys::vm_map(
            process,
            PERM_READ | PERM_WRITE,
            0,
            mem.raw(),
            0,
            end - start,
            &mut mapped,
        )
    };
    if status != mikros_rt::OK {
        return Err(Status::from_raw(status as i32));
    }
    let first = mapped + (bar.base + offset - start);
    Ok((first, mapped, end - start))
}

/// The `MSI` resource that the bus driver asks for once.
async fn msi_resource(bus: &Bus) -> Result<OwnedHandle, Status> {
    if let Some(resource) = bus.msi_resource.borrow().as_ref() {
        return duplicate(resource, 1 << 31);
    }
    let granted = bus.manager.request_resource(KIND_MSI, 0, 0, false).await;
    let resource = match granted {
        Ok(Ok(granted)) => granted.resource,
        Ok(Err(status)) => return Err(status),
        Err(_) => return Err(Status::PeerClosed),
    };
    let copy = duplicate(&resource, 1 << 31)?;
    *bus.msi_resource.borrow_mut() = Some(resource);
    Ok(copy)
}

fn event_create() -> Result<OwnedHandle, Status> {
    let mut event = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::event_create(0, &mut event) };
    if status != mikros_rt::OK {
        return Err(Status::from_raw(status as i32));
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(event) })
}

fn duplicate(handle: &OwnedHandle, rights: u32) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle.raw(), rights, &mut out) };
    if status != mikros_rt::OK {
        return Err(Status::from_raw(status as i32));
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}
