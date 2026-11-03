//! Per-processor data, reached through the GS base.

use crate::aspace::Pcids;
use crate::cpu::write_msr;
use crate::gdt::{Gdt, Tss};
use crate::sched::CpuSched;
use alloc::vec::Vec;
use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// The top of the running user thread's kernel stack, which the
    /// system call entry switches to.
    pub kernel_stack: AtomicU64,
    /// Where the system call entry keeps the user stack pointer until it
    /// is on the kernel stack.
    user_rsp: AtomicU64,
    /// The coverage buffer of the running thread in kernel space, or 0
    /// when it collects none, and the number of addresses it holds.
    pub cov_area: AtomicU64,
    pub cov_capacity: AtomicU64,
    /// The depth of hardware interrupt handling, during which no
    /// coverage is collected.
    interrupts: AtomicU64,
    /// The scheduler state of the processor.
    pub sched: SpinLock<CpuSched>,
    /// The address spaces that have PCIDs on the processor.
    pub pcids: SpinLock<Pcids>,
    /// The top of the kernel stack of the thread that last ran, user or
    /// kernel, for backtraces.
    pub thread_stack: AtomicU64,
    /// Set by the timer interrupt when the running thread's slice has
    /// ended.
    pub reschedule: AtomicBool,
    /// Whether the debug registers hold the breakpoints of a user thread.
    pub breakpoints_loaded: AtomicBool,
    /// The I/O port ranges the bitmap of `tss` allows now. Its capacity,
    /// which never shrinks, covers the ranges of every process, so that
    /// switching them in needs no allocation. Locked before a process's
    /// ports.
    pub open_ports: SpinLock<Vec<(u64, u64)>>,
    /// The processor's task state segment and GDT, which the processor
    /// reads as it runs and `gdt` changes.
    pub tss: UnsafeCell<Tss>,
    pub gdt: UnsafeCell<Gdt>,
}

// SAFETY: each processor only reads its own structure, `this` is never
// written after setup, and the rest is behind locks or atomic, except
// for the TSS and the GDT, which only their own processor uses.
unsafe impl Sync for PerCpu {}

/// The boot processor's data. Entry code that cannot trust the GS base
/// finds it by this symbol.
pub(crate) static BOOT_CPU: PerCpu = PerCpu {
    this: &raw const BOOT_CPU,
    index: 0,
    kernel_stack: AtomicU64::new(0),
    user_rsp: AtomicU64::new(0),
    cov_area: AtomicU64::new(0),
    cov_capacity: AtomicU64::new(0),
    interrupts: AtomicU64::new(0),
    sched: SpinLock::new(CpuSched::new()),
    pcids: SpinLock::new(Pcids::new()),
    thread_stack: AtomicU64::new(0),
    reschedule: AtomicBool::new(false),
    breakpoints_loaded: AtomicBool::new(false),
    open_ports: SpinLock::new(Vec::new()),
    tss: UnsafeCell::new(Tss::new()),
    gdt: UnsafeCell::new([0; 8]),
};

/// Set once the GS base points to the per-CPU data.
pub static READY: AtomicBool = AtomicBool::new(false);

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

/// The user stack pointer saved by the last system call entry on this
/// processor.
pub fn user_rsp() -> u64 {
    this().user_rsp.load(Ordering::Relaxed)
}

/// The offsets that entry code uses.
pub const KERNEL_STACK_OFFSET: usize = 16;
pub const USER_RSP_OFFSET: usize = 24;
pub const COV_AREA_OFFSET: usize = 32;
pub const COV_CAPACITY_OFFSET: usize = 40;
pub const INTERRUPTS_OFFSET: usize = 48;

const _: () = assert!(core::mem::offset_of!(PerCpu, index) == 8);
const _: () = assert!(core::mem::offset_of!(PerCpu, kernel_stack) == KERNEL_STACK_OFFSET);
const _: () = assert!(core::mem::offset_of!(PerCpu, user_rsp) == USER_RSP_OFFSET);
const _: () = assert!(core::mem::offset_of!(PerCpu, cov_area) == COV_AREA_OFFSET);
const _: () = assert!(core::mem::offset_of!(PerCpu, cov_capacity) == COV_CAPACITY_OFFSET);
const _: () = assert!(core::mem::offset_of!(PerCpu, interrupts) == INTERRUPTS_OFFSET);
