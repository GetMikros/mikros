//! Switching between kernel stacks, and a thread's extended state.

use alloc::alloc::{Layout, alloc_zeroed, dealloc};
use core::arch::{asm, global_asm};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

// Saves the callee-saved registers on the current stack, stores the stack
// pointer at RDI, loads RSI as the stack pointer, and restores the
// registers saved there. Returns on the new stack.
global_asm!(
    ".global context_switch",
    "context_switch:",
    "push rbp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rdi], rsp",
    "mov rsp, rsi",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "ret",
    // A new thread's first switch returns here, with the function in R12
    // and its argument in R13, and the stack aligned for a call.
    ".global context_start",
    "context_start:",
    "mov rdi, r12",
    "mov rsi, r13",
    "call {start}",
    "ud2",
    start = sym crate::sched::thread_start,
);

unsafe extern "sysv64" {
    fn context_switch(save: *mut u64, load: u64);
    fn context_start();
}

/// Switches to the stack saved in `load`, saving the current one in
/// `save`. Returns when something switches back.
///
/// # Safety
///
/// `load` must be a stack pointer saved by `context_switch` or prepared
/// by `prepare`, whose stack is mapped and used by nothing else.
pub unsafe fn switch(save: *mut u64, load: u64) {
    // SAFETY: the caller guarantees `load` is a switchable stack.
    unsafe { context_switch(save, load) };
}

/// Prepares the stack whose top is `top`, aligned to 16 bytes, so that
/// switching to the returned stack pointer calls `entry(arg)`.
///
/// # Safety
///
/// The 64 bytes below `top` must be mapped and unused.
pub unsafe fn prepare(top: u64, entry: u64, arg: u64) -> u64 {
    // From the lowest address: R15, R14, R13, R12, RBX, RBP, the return
    // address, then two zero words, which leave the stack aligned after
    // the return and end backtraces.
    let frame = [
        0,
        0,
        arg,
        entry,
        0,
        0,
        context_start as *const () as u64,
        0,
        0,
    ];
    let rsp = top - 8 * frame.len() as u64;
    // SAFETY: the caller guarantees the words below `top` are writable.
    unsafe {
        core::ptr::copy_nonoverlapping(frame.as_ptr(), rsp as *mut u64, frame.len());
    }
    rsp
}

const MXCSR_OFFSET: usize = 24;
/// Every SIMD floating-point exception masked, round to nearest.
const MXCSR_RESET: u32 = 0x1F80;

/// The size of the XSAVE area for the enabled state components.
static XSAVE_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Reads the XSAVE area size. XCR0 must be set up.
pub fn init() {
    let size = core::arch::x86_64::__cpuid_count(0xD, 0).ebx as usize;
    XSAVE_SIZE.store(size, Ordering::Relaxed);
}

fn xsave_layout() -> Layout {
    let size = XSAVE_SIZE.load(Ordering::Relaxed).max(576);
    Layout::from_size_align(size, 64).unwrap_or_else(|_| panic!("bad XSAVE area size {size}"))
}

/// A thread's extended state, in XSAVE format.
pub struct ExtendedState(NonNull<u8>);

// SAFETY: the area is owned by one thread's state and only touched while
// switching to or from that thread.
unsafe impl Send for ExtendedState {}
unsafe impl Sync for ExtendedState {}

impl ExtendedState {
    /// An area in the initial state. Its header marks every component as
    /// initial, so XRSTOR loads their initial values, except MXCSR, which
    /// XRSTOR always takes from the area and which is set to its reset
    /// value.
    pub fn new() -> Option<ExtendedState> {
        // SAFETY: the layout has a nonzero size.
        let area = NonNull::new(unsafe { alloc_zeroed(xsave_layout()) })?;
        // SAFETY: MXCSR is at offset 24 of the legacy region, inside the
        // area and aligned.
        unsafe { area.add(MXCSR_OFFSET).cast::<u32>().write(MXCSR_RESET) };
        Some(ExtendedState(area))
    }

    /// Saves the processor's extended state here.
    pub fn save(&self) {
        // SAFETY: the area is 64-byte aligned and large enough for every
        // enabled component; EDX:EAX selects all of them.
        unsafe {
            asm!("xsave64 [{}]", in(reg) self.0.as_ptr(), in("eax") u32::MAX, in("edx") u32::MAX, options(nostack, preserves_flags))
        };
    }

    /// Loads the extended state saved here, or the initial state.
    pub fn restore(&self) {
        // SAFETY: as for `save`; the area holds a valid XSAVE image or
        // zeros.
        unsafe {
            asm!("xrstor64 [{}]", in(reg) self.0.as_ptr(), in("eax") u32::MAX, in("edx") u32::MAX, options(nostack, preserves_flags, readonly))
        };
    }
}

impl Drop for ExtendedState {
    fn drop(&mut self) {
        // SAFETY: the area came from `alloc_zeroed` with this layout.
        unsafe { dealloc(self.0.as_ptr(), xsave_layout()) };
    }
}
