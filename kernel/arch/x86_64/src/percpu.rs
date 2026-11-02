//! Per-processor data, reached through the GS base.

use crate::aspace::Pcids;
use crate::cpu::write_msr;
use crate::sched::CpuSched;
use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};
use kernel_core::sync::SpinLock;

pub const IA32_GS_BASE: u32 = 0xC000_0101;
pub const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;

/// The data of one processor. The layout is fixed because entry code
/// reaches the fields at their offsets from the GS base.
#[repr(C)]
pub struct PerCpu {
    /// The address of this structure.
    this: *const PerCpu,
    /// The processor's index, from 0.
    index: u32,
    /// The scheduler state of the processor.
    pub sched: SpinLock<CpuSched>,
    /// The address spaces that have PCIDs on the processor.
    pub pcids: SpinLock<Pcids>,
}

// SAFETY: each processor only reads its own structure, `this` is never
// written after setup, and the rest is behind locks.
unsafe impl Sync for PerCpu {}

static BOOT_CPU: PerCpu = PerCpu {
    this: &raw const BOOT_CPU,
    index: 0,
    sched: SpinLock::new(CpuSched::new()),
    pcids: SpinLock::new(Pcids::new()),
};

static READY: AtomicBool = AtomicBool::new(false);

/// Points the GS base at the boot processor's data, and the user GS base
/// at zero.
pub fn init() {
    // SAFETY: both MSRs exist in long mode, and nothing else uses the GS
    // base.
    unsafe {
        write_msr(IA32_GS_BASE, (&raw const BOOT_CPU) as u64);
        write_msr(IA32_KERNEL_GS_BASE, 0);
    }
    READY.store(true, Ordering::Release);
}

/// This processor's index, or 0 before `init`.
pub fn index() -> u32 {
    if !READY.load(Ordering::Acquire) {
        return 0;
    }
    let index: u32;
    // SAFETY: the GS base points at this processor's PerCpu, whose index
    // field is at offset 8.
    unsafe {
        asm!(
            "mov {:e}, gs:[8]",
            out(reg) index,
            options(readonly, nostack, preserves_flags),
        )
    };
    index
}

/// This processor's data. `init` must have run.
pub fn this() -> &'static PerCpu {
    let this: *const PerCpu;
    // SAFETY: the GS base points at this processor's PerCpu, whose first
    // field holds its address.
    unsafe {
        asm!(
            "mov {}, gs:[0]",
            out(reg) this,
            options(readonly, nostack, preserves_flags),
        )
    };
    // SAFETY: the address is that of a static PerCpu.
    unsafe { &*this }
}

const _: () = assert!(core::mem::offset_of!(PerCpu, index) == 8);
