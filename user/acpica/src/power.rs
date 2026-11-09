//! Powering the machine off and restarting it, and the power button,
//! through the glue, with the resources of the registers that they write
//! held from the start.

use crate::sync::now;
use crate::{AE_ERROR, AE_OK, Status, physical, ports};
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use mikros_rt::sys;

/// The reset control register, and the value that resets the machine.
const RESET_CONTROL: u16 = 0xCF9;
const RESET: u32 = 0x06;
/// How long a reset through the FADT's register may take before the
/// reset control register is written.
const RESET_WAIT: i64 = 1_000_000_000;
/// The power registers of `mikros_acpi_power_register`: the PM1a and PM1b
/// control registers and the reset register, and their address spaces.
const POWER_REGISTERS: u32 = 3;
const SPACE_IO: u32 = 1;

unsafe extern "C" {
    fn mikros_acpi_power_off() -> Status;
    fn mikros_acpi_reset_supported() -> u32;
    fn mikros_acpi_reset() -> Status;
    fn mikros_acpi_power_register(
        index: u32,
        space: *mut u32,
        address: *mut u64,
        bytes: *mut u32,
    ) -> Status;
    fn mikros_acpi_watch_power_button(
        pressed: extern "C" fn(*mut c_void),
        context: *mut c_void,
    ) -> Status;
}

/// Obtains and keeps the resources of the FADT's PM1a and PM1b control
/// registers, of its reset register when it reports one, and of the reset
/// control register: their ports stay enabled and their memory mapped, so
/// that the machine can be powered off and restarted once no more
/// resources can be had. One that cannot be had is logged.
pub(crate) fn hold_resources() {
    for index in 0..POWER_REGISTERS {
        let (mut space, mut address, mut bytes) = (0, 0, 0);
        // SAFETY: the pointers are valid for the call.
        let found =
            unsafe { mikros_acpi_power_register(index, &mut space, &mut address, &mut bytes) };
        if found != AE_OK {
            continue;
        }
        let held = match space {
            SPACE_IO => u16::try_from(address)
                .ok()
                .zip(u16::try_from(bytes).ok())
                .is_some_and(|(port, count)| ports::enable(port, count)),
            // The mapping is never given back.
            _ => physical::map(address, u64::from(bytes)).is_some(),
        };
        if !held {
            mikros_rt::log!(
                Warning,
                "acpi: holding power register {index} at {address:#x} failed"
            );
        }
    }
    if !ports::enable(RESET_CONTROL, 1) {
        mikros_rt::log!(Warning, "acpi: holding port {RESET_CONTROL:#x} failed");
    }
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
