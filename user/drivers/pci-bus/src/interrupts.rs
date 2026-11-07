//! The interrupts of the functions: INTx lines, which the bus driver
//! holds and shares between the functions on them.

use crate::bus::{Bus, Function};
use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_pci::{COMMAND, Config, STATUS};
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::acpi::{Acpi, AcpiAsyncClient};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;

pub const KIND_IRQ: u32 = 3;
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
const WAIT: u32 = 1 << 6;
const INSPECT: u32 = 1 << 9;

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

/// The line of `route`, which the bus driver takes, from an `IRQ`
/// resource, and watches, the first time a function is on it.
async fn line(bus: &Rc<Bus>, route: &Route) -> Result<Rc<Line>, Status> {
    if let Some(line) = bus.lines.borrow().get(&route.gsi) {
        return Ok(line.clone());
    }
    let granted = bus
        .manager
        .request_resource(KIND_IRQ, u64::from(route.gsi), 1, false)
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
