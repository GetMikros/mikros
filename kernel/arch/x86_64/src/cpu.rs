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

/// Writes `value` to the model-specific register `msr`.
///
/// # Safety
///
/// The register must exist, and the value must not break an invariant the
/// kernel relies on.
pub unsafe fn write_msr(msr: u32, value: u64) {
    // SAFETY: the caller guarantees the write is valid and harmless.
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nostack, preserves_flags),
        )
    };
}

pub fn read_cr4() -> u64 {
    let value;
    // SAFETY: reading CR4 has no side effects.
    unsafe { asm!("mov {}, cr4", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Writes CR4.
///
/// # Safety
///
/// The value must keep the processor in the mode the kernel runs in.
pub unsafe fn write_cr4(value: u64) {
    // SAFETY: the caller guarantees the value is valid.
    unsafe { asm!("mov cr4, {}", in(reg) value, options(nostack, preserves_flags)) };
}

/// Reads 32 bits from the I/O port `port`.
///
/// # Safety
///
/// The read must not break an invariant of the device at `port`.
pub unsafe fn in32(port: u16) -> u32 {
    let value;
    // SAFETY: the caller guarantees the read is harmless; `in` touches no
    // memory.
    unsafe { asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack)) };
    value
}

/// The time stamp counter.
pub fn rdtsc() -> u64 {
    // SAFETY: the TSC is a required feature and CR4.TSD is clear, so RDTSC
    // only reads the counter.
    unsafe { core::arch::x86_64::_rdtsc() }
}
