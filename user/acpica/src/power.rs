//! Powering the machine off and restarting it, and the power button,
//! through the glue.

use crate::sync::now;
use crate::{AE_ERROR, AE_OK, Status, ports};
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use mikros_rt::sys;

/// The reset control register, and the value that resets the machine.
const RESET_CONTROL: u16 = 0xCF9;
const RESET: u32 = 0x06;
/// How long a reset through the FADT's register may take before the
/// reset control register is written.
const RESET_WAIT: i64 = 1_000_000_000;

unsafe extern "C" {
    fn mikros_acpi_power_off() -> Status;
    fn mikros_acpi_reset_supported() -> u32;
    fn mikros_acpi_reset() -> Status;
    fn mikros_acpi_watch_power_button(
        pressed: extern "C" fn(*mut c_void),
        context: *mut c_void,
    ) -> Status;
}

/// Enters sleep state S5, which does not return: the status of the step
/// that failed.
pub fn power_off() -> Status {
    // SAFETY: the glue takes no arguments.
    unsafe { mikros_acpi_power_off() }
}

/// Resets the machine through the FADT's reset register when the FADT
/// says that it has one, and through the reset control register when it
/// has none or the machine still runs a second later. Returns only when
/// neither reset the machine.
pub fn restart() -> Status {
    // SAFETY: the glue takes no arguments.
    if unsafe { mikros_acpi_reset_supported() } != 0 {
        // SAFETY: as above.
        if unsafe { mikros_acpi_reset() } == AE_OK {
            // SAFETY: the call takes no pointers.
            unsafe { sys::thread_sleep(now() + RESET_WAIT) };
        }
    }
    ports::write(RESET_CONTROL, RESET, 8);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_sleep(now() + RESET_WAIT) };
    AE_ERROR
}

/// The function that the power button calls, 0 until one is watched.
static PRESSED: AtomicUsize = AtomicUsize::new(0);

extern "C" fn pressed(_context: *mut c_void) {
    let pressed = PRESSED.load(Ordering::Acquire);
    if pressed != 0 {
        // SAFETY: `watch_power_button` stored a `fn()`.
        let pressed: fn() = unsafe { core::mem::transmute(pressed) };
        pressed();
    }
}

/// Calls `on_press` each time the fixed power button event or a power
/// button device's notification comes, on a thread of the operating
/// system layer.
pub fn watch_power_button(on_press: fn()) -> Result<(), Status> {
    PRESSED.store(on_press as usize, Ordering::Release);
    // SAFETY: `pressed` takes no context.
    match unsafe { mikros_acpi_watch_power_button(pressed, core::ptr::null_mut()) } {
        AE_OK => Ok(()),
        status => Err(status),
    }
}
