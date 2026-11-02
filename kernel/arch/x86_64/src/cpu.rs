//! Processor control.

use core::arch::asm;

/// Stops the processor with interrupts disabled.
pub fn halt() -> ! {
    loop {
        // SAFETY: disabling interrupts and halting touch no memory; with
        // interrupts disabled the processor stays halted.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

/// Writes `value` to the I/O port `port`.
///
/// # Safety
///
/// The write must not break an invariant of the device at `port`.
pub unsafe fn out8(port: u16, value: u8) {
    // SAFETY: the caller guarantees the write is harmless; `out` touches no
    // memory.
    unsafe { asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack)) };
}

/// Reads from the I/O port `port`.
///
/// # Safety
///
/// The read must not break an invariant of the device at `port`.
pub unsafe fn in8(port: u16) -> u8 {
    let value;
    // SAFETY: the caller guarantees the read is harmless; `in` touches no
    // memory.
    unsafe { asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack)) };
    value
}
