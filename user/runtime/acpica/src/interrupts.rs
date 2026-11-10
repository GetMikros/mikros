//! The system control interrupt for ACPICA: an `Interrupt` object for the
//! GSI of the FADT's SCI number, level-triggered and active low unless an
//! interrupt source override of the MADT says otherwise, and a thread
//! that waits for it and runs ACPICA's handler each time it comes.

use crate::sync::{Locked, now};
use crate::{AE_ALREADY_EXISTS, AE_BAD_PARAMETER, AE_ERROR, AE_NOT_EXIST, AE_OK, Status};
use alloc::boxed::Box;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const KIND_IRQ: u32 = 3;
/// `interrupt_create` options, and the signal of a raised interrupt.
const LEVEL: u32 = 1 << 0;
const ACTIVE_LOW: u32 = 1 << 1;
const SIGNALED: u32 = 1 << 3;
/// The flags of `mikros_acpi_sci`.
const SCI_LEVEL: u32 = 1 << 0;
const SCI_ACTIVE_LOW: u32 = 1 << 1;
/// How often the thread looks whether its handler was removed.
const CHECK_EVERY: i64 = 1_000_000_000;

/// ACPICA's handler of an interrupt.
type Handler = unsafe extern "C" fn(*mut c_void) -> u32;

/// The SCI's number in the FADT, its GSI, and its `interrupt_create`
/// options.
#[derive(Clone, Copy)]
struct Sci {
    number: u32,
    gsi: u32,
    options: u32,
}

static SCI: Locked<Option<Sci>> = Locked::new(None);
/// Whether a handler is installed, and that it should stop.
static INSTALLED: AtomicBool = AtomicBool::new(false);
static REMOVED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    fn mikros_acpi_sci(number: *mut u32, gsi: *mut u32, flags: *mut u32) -> Status;
}

/// Reads the SCI's number, GSI, trigger mode and polarity, once the
/// tables are found.
pub fn read_sci() -> Result<(), Status> {
    let (mut number, mut gsi, mut flags) = (0, 0, 0);
    // SAFETY: the pointers are valid for the call.
    let status = unsafe { mikros_acpi_sci(&mut number, &mut gsi, &mut flags) };
    if status != AE_OK {
        return Err(status);
    }
    let options = if flags & SCI_LEVEL != 0 { LEVEL } else { 0 }
        | if flags & SCI_ACTIVE_LOW != 0 {
            ACTIVE_LOW
        } else {
            0
        };
    *SCI.lock() = Some(Sci {
        number,
        gsi,
        options,
    });
    Ok(())
}

/// What the SCI's thread runs.
struct Waiting {
    handler: Handler,
    context: usize,
    interrupt: OwnedHandle,
    _resource: OwnedHandle,
}

fn wait(waiting: usize) {
    // SAFETY: `AcpiOsInstallInterruptHandler` passes a box for this
    // thread alone.
    let waiting = unsafe { Box::from_raw(waiting as *mut Waiting) };
    while !REMOVED.load(Ordering::Acquire) {
        let mut observed = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe {
            sys::object_wait_one(
                waiting.interrupt.raw(),
                SIGNALED,
                now() + CHECK_EVERY,
                &mut observed,
            )
        };
        if status != mikros_rt::OK || observed & SIGNALED == 0 {
            continue;
        }
        // SAFETY: ACPICA installed the handler with its context.
        unsafe { (waiting.handler)(waiting.context as *mut c_void) };
        // SAFETY: the call takes no pointers.
        unsafe { sys::interrupt_ack(waiting.interrupt.raw()) };
    }
    INSTALLED.store(false, Ordering::Release);
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsInstallInterruptHandler(
    number: u32,
    handler: Option<Handler>,
    context: *mut c_void,
) -> Status {
    let Some(handler) = handler else {
        return AE_BAD_PARAMETER;
    };
    let Some(sci) = *SCI.lock() else {
        return AE_NOT_EXIST;
    };
    // Only the SCI has a handler.
    if number != sci.number {
        return AE_BAD_PARAMETER;
    }
    if INSTALLED.swap(true, Ordering::AcqRel) {
        return AE_ALREADY_EXISTS;
    }
    REMOVED.store(false, Ordering::Release);
    let installed = install(sci, handler, context);
    if installed != AE_OK {
        INSTALLED.store(false, Ordering::Release);
    }
    installed
}

fn install(sci: Sci, handler: Handler, context: *mut c_void) -> Status {
    let gsi = u64::from(sci.gsi);
    let Some(resource) = crate::host().request(KIND_IRQ, gsi, 1, false) else {
        return AE_ERROR;
    };
    let mut interrupt = 0;
    // SAFETY: the out pointer is valid for the call.
    let created =
        unsafe { sys::interrupt_create(resource.raw(), sci.gsi, sci.options, &mut interrupt) };
    if created != mikros_rt::OK {
        return AE_ERROR;
    }
    let waiting = Box::into_raw(Box::new(Waiting {
        handler,
        context: context as usize,
        // SAFETY: the kernel just created the handle for us.
        interrupt: unsafe { OwnedHandle::from_raw(interrupt) },
        _resource: resource,
    }));
    if crate::threads::spawn(crate::process(), b"acpi-sci", wait, waiting as usize) {
        AE_OK
    } else {
        // SAFETY: the thread did not start, so the box is still ours.
        drop(unsafe { Box::from_raw(waiting) });
        AE_ERROR
    }
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsRemoveInterruptHandler(number: u32, _handler: Option<Handler>) -> Status {
    match *SCI.lock() {
        Some(sci) if sci.number == number && INSTALLED.load(Ordering::Acquire) => {
            REMOVED.store(true, Ordering::Release);
            AE_OK
        }
        _ => AE_NOT_EXIST,
    }
}
