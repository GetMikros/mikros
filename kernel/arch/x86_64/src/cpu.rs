//! Processor control.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};

/// Whether INVPCID is used: reported, with PCIDs enabled.
static INVPCID: AtomicBool = AtomicBool::new(false);

/// Whether SMAP is reported, so that STAC and CLAC may be used. The
/// interrupt entry reads it as a byte.
pub static SMAP: AtomicBool = AtomicBool::new(false);

/// RFLAGS.AC, which lets the kernel reach user pages while SMAP is on.
pub const RFLAGS_AC: u64 = 1 << 18;

/// The current RFLAGS.
pub fn rflags() -> u64 {
    let value;
    // SAFETY: PUSHFQ and POP only move RFLAGS through the stack.
    unsafe { asm!("pushfq", "pop {}", out(reg) value, options(nomem, preserves_flags)) };
    value
}

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

/// Reads the model-specific register `msr`.
///
/// # Safety
///
/// The register must exist, and reading it must have no side effect the
/// kernel relies on not happening.
pub unsafe fn read_msr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: the caller guarantees the read is valid and harmless.
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        )
    };
    u64::from(high) << 32 | u64::from(low)
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

/// The address of the last page fault.
pub fn read_cr2() -> u64 {
    let value;
    // SAFETY: reading CR2 has no side effects.
    unsafe { asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

pub fn read_cr0() -> u64 {
    let value;
    // SAFETY: reading CR0 has no side effects.
    unsafe { asm!("mov {}, cr0", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Writes CR0.
///
/// # Safety
///
/// The value must keep the processor in the mode the kernel runs in.
pub unsafe fn write_cr0(value: u64) {
    // SAFETY: the caller guarantees the value is valid.
    unsafe { asm!("mov cr0, {}", in(reg) value, options(nostack, preserves_flags)) };
}

/// Writes the extended control register `xcr`.
///
/// # Safety
///
/// CR4.OSXSAVE must be set, and the value must be valid for the register.
pub unsafe fn write_xcr(xcr: u32, value: u64) {
    // SAFETY: the caller guarantees the write is valid.
    unsafe {
        asm!(
            "xsetbv",
            in("ecx") xcr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags),
        )
    };
}

const IA32_EFER: u32 = 0xC000_0080;

/// Sets up CR0, EFER, CR4 and XCR0. The required features must be
/// present.
pub fn init() {
    use crate::control::{self, CR4_LA57, Optional};
    let features = Optional::read(|leaf, subleaf| {
        let r = core::arch::x86_64::__cpuid_count(leaf, subleaf);
        [r.eax, r.ebx, r.ecx, r.edx]
    });
    // CR4.LA57 cannot change in long mode; the bootloader leaves it clear.
    if read_cr4() & CR4_LA57 != 0 {
        panic!("5-level paging is enabled");
    }
    // SAFETY: the new values keep paging, protected mode and long mode
    // on, and set only bits for features that are required or reported.
    // PCIDE may be set because the low bits of CR3 are clear, and XCR0 is
    // written after OSXSAVE is set, with components that XSAVE, AVX and
    // AVX-512F imply.
    unsafe {
        write_cr0(control::cr0(read_cr0()));
        write_msr(IA32_EFER, control::efer(read_msr(IA32_EFER)));
        write_cr4(control::cr4(read_cr4(), features));
        write_xcr(0, control::xcr0(features));
    }
    INVPCID.store(features.pcid && features.invpcid, Ordering::Relaxed);
    SMAP.store(features.smap, Ordering::Relaxed);
}

/// Whether `invpcid_address` and `invpcid_context` may be used.
pub fn has_invpcid() -> bool {
    INVPCID.load(Ordering::Relaxed)
}

/// Runs INVPCID of `kind` for `pcid` and `virt`.
fn invpcid(kind: u64, pcid: u64, virt: u64) {
    let descriptor = [pcid, virt];
    // SAFETY: `has_invpcid` holds; INVPCID only drops cached translations
    // and reads the 16-byte descriptor.
    unsafe {
        asm!(
            "invpcid {}, [{}]",
            in(reg) kind,
            in(reg) descriptor.as_ptr(),
            options(nostack, preserves_flags, readonly),
        )
    };
}

/// Invalidates the translation of the page that contains `virt` under
/// `pcid`, which need not be loaded. `has_invpcid` must hold.
pub fn invpcid_address(pcid: u64, virt: u64) {
    invpcid(0, pcid, virt);
}

/// Invalidates every translation under `pcid` other than global ones.
/// `has_invpcid` must hold.
pub fn invpcid_context(pcid: u64) {
    invpcid(1, pcid, 0);
}

/// Invalidates the translations of the page that contains `virt`,
/// including a global one.
pub fn invlpg(virt: u64) {
    // SAFETY: INVLPG only drops cached translations.
    unsafe { asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags)) };
}

/// Loads CR3.
///
/// # Safety
///
/// The table must map the kernel half like the kernel's own tables.
pub unsafe fn write_cr3(value: u64) {
    // SAFETY: the caller guarantees the kernel stays mapped.
    unsafe { asm!("mov cr3, {}", in(reg) value, options(nostack, preserves_flags)) };
}

/// Reads debug register `n`, one of 0 to 3, 6 or 7.
pub fn read_dr(n: u8) -> u64 {
    let value;
    // SAFETY: reading a debug register has no side effects, and CR4.DE is
    // clear, so DR4 and DR5 are never named.
    unsafe {
        match n {
            0 => asm!("mov {}, dr0", out(reg) value, options(nomem, nostack, preserves_flags)),
            1 => asm!("mov {}, dr1", out(reg) value, options(nomem, nostack, preserves_flags)),
            2 => asm!("mov {}, dr2", out(reg) value, options(nomem, nostack, preserves_flags)),
            3 => asm!("mov {}, dr3", out(reg) value, options(nomem, nostack, preserves_flags)),
            6 => asm!("mov {}, dr6", out(reg) value, options(nomem, nostack, preserves_flags)),
            _ => asm!("mov {}, dr7", out(reg) value, options(nomem, nostack, preserves_flags)),
        }
    }
    value
}

/// Writes debug register `n`, one of 0 to 3, 6 or 7.
///
/// # Safety
///
/// Addresses must be canonical, DR6 and DR7 must have their upper 32 bits
/// clear, and DR7 must not enable I/O or kernel breakpoints.
pub unsafe fn write_dr(n: u8, value: u64) {
    // SAFETY: the caller guarantees the value is valid.
    unsafe {
        match n {
            0 => asm!("mov dr0, {}", in(reg) value, options(nomem, nostack, preserves_flags)),
            1 => asm!("mov dr1, {}", in(reg) value, options(nomem, nostack, preserves_flags)),
            2 => asm!("mov dr2, {}", in(reg) value, options(nomem, nostack, preserves_flags)),
            3 => asm!("mov dr3, {}", in(reg) value, options(nomem, nostack, preserves_flags)),
            6 => asm!("mov dr6, {}", in(reg) value, options(nomem, nostack, preserves_flags)),
            _ => asm!("mov dr7, {}", in(reg) value, options(nomem, nostack, preserves_flags)),
        }
    }
}
