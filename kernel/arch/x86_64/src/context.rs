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

// Enters user mode at RDI with stack pointer RSI, RDX and RCX as the
// first two arguments, RFLAGS 0x202 and every other general-purpose
// register zero. The user GS base must be in IA32_KERNEL_GS_BASE.
global_asm!(
    ".global context_enter_user",
    "context_enter_user:",
    "push {user_data}",
    "push rsi",
    "push 0x202",
    "push {user_code}",
    "push rdi",
    "mov rdi, rdx",
    "mov rsi, rcx",
    "xor eax, eax",
    "xor ebx, ebx",
    "xor ecx, ecx",
    "xor edx, edx",
    "xor ebp, ebp",
    "xor r8d, r8d",
    "xor r9d, r9d",
    "xor r10d, r10d",
    "xor r11d, r11d",
    "xor r12d, r12d",
    "xor r13d, r13d",
    "xor r14d, r14d",
    "xor r15d, r15d",
    "swapgs",
    "iretq",
    user_data = const crate::gdt::USER_DATA as u64 | 3,
    user_code = const crate::gdt::USER_CODE as u64 | 3,
);

// Leaves for user mode with every register taken from the interrupt
// frame at RDI. The user GS base must be in IA32_KERNEL_GS_BASE.
global_asm!(
    ".global context_enter_user_frame",
    "context_enter_user_frame:",
    "mov rsp, rdi",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rbp",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop rcx",
    "pop rbx",
    "pop rax",
    "add rsp, 16",
    "swapgs",
    "iretq",
);

unsafe extern "sysv64" {
    fn context_enter_user_frame(frame: *const crate::idt::Frame) -> !;
    fn context_switch(save: *mut u64, load: u64);
    fn context_start();
    fn context_enter_user(rip: u64, rsp: u64, arg1: u64, arg2: u64) -> !;
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

/// Leaves the kernel for user mode with the registers in `frame`, which
/// lies on the running thread's kernel stack and is abandoned with it.
///
/// # Safety
///
/// As for `enter_user`; `frame` must hold user segment selectors and a
/// RIP in the user range.
pub unsafe fn enter_user_frame(frame: &crate::idt::Frame) -> ! {
    // SAFETY: the caller guarantees the frame and the user state.
    unsafe { context_enter_user_frame(frame) }
}

/// Leaves the kernel for user mode at `rip` with stack pointer `rsp`
/// and the two arguments.
///
/// # Safety
///
/// The running thread's address space must be loaded, its extended state
/// restored, and its user FS and GS bases set.
pub unsafe fn enter_user(rip: u64, rsp: u64, arg1: u64, arg2: u64) -> ! {
    // SAFETY: the caller guarantees the thread's user state is in place;
    // anything the return faults on is in user mode.
    unsafe { context_enter_user(rip, rsp, arg1, arg2) }
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

/// The size of a thread's extended state area.
pub fn extended_state_size() -> usize {
    xsave_layout().size()
}

fn xsave_layout() -> Layout {
    let size = XSAVE_SIZE.load(Ordering::Relaxed).max(576);
    Layout::from_size_align(size, 64).unwrap_or_else(|_| panic!("bad XSAVE area size {size}"))
}

/// The state components enabled in XCR0.
pub fn xcr0() -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: XSAVE is a required feature, so XGETBV with ECX 0 reads
    // XCR0.
    unsafe {
        asm!("xgetbv", in("ecx") 0, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags))
    };
    u64::from(high) << 32 | u64::from(low)
}

/// A thread's extended state, in XSAVE format.
pub struct ExtendedState(NonNull<u8>);

// SAFETY: the area is owned by one thread's state and only touched while
// switching to or from that thread.
unsafe impl Send for ExtendedState {}
// SAFETY: as for `Send`; no two threads touch the area at once.
unsafe impl Sync for ExtendedState {}

impl ExtendedState {
    /// An area in the initial state, as `init_area` sets it.
    pub fn new() -> Option<ExtendedState> {
        // SAFETY: the layout has a nonzero size.
        let area = NonNull::new(unsafe { alloc_zeroed(xsave_layout()) })?;
        // SAFETY: the area is zeroed and has the XSAVE layout.
        unsafe { init_area(area.as_ptr()) };
        Some(ExtendedState(area))
    }

    /// The saved XSAVE image.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the area is allocated with this size, and the thread it
        // belongs to does not run while it is read.
        unsafe { core::slice::from_raw_parts(self.0.as_ptr(), extended_state_size()) }
    }

    /// The saved XSAVE image, to be replaced while its thread does not
    /// run. What is written must be loadable with XRSTOR.
    #[expect(
        clippy::mut_from_ref,
        reason = "the area is owned by one thread that does not run"
    )]
    pub fn bytes_mut(&self) -> &mut [u8] {
        // SAFETY: as for `bytes`; the caller has nothing else referencing
        // the area.
        unsafe { core::slice::from_raw_parts_mut(self.0.as_ptr(), extended_state_size()) }
    }

    /// Saves the processor's extended state here.
    pub fn save(&self) {
        // SAFETY: the area has the XSAVE layout.
        unsafe { save_area(self.0.as_ptr()) };
    }

    /// Loads the extended state saved here, or the initial state.
    pub fn restore(&self) {
        // SAFETY: the area holds an initial or saved XSAVE image.
        unsafe { restore_area(self.0.as_ptr()) };
    }
}

/// The layout of an XSAVE area for the enabled state components.
pub fn area_layout() -> Layout {
    xsave_layout()
}

/// Puts the initial state in the zeroed XSAVE area `area`. Its header
/// marks every component as initial, so XRSTOR loads their initial
/// values, except MXCSR, which XRSTOR always takes from the area and
/// which is set to its reset value.
///
/// # Safety
///
/// `area` must be zeroed and have the layout `area_layout` gives.
pub unsafe fn init_area(area: *mut u8) {
    // SAFETY: MXCSR is at offset 24 of the legacy region, inside the area
    // and aligned.
    unsafe { area.add(MXCSR_OFFSET).cast::<u32>().write(MXCSR_RESET) };
}

/// Saves the processor's extended state in `area`.
///
/// # Safety
///
/// `area` must have the layout `area_layout` gives.
pub unsafe fn save_area(area: *mut u8) {
    // SAFETY: the area is 64-byte aligned and large enough for every
    // enabled component; EDX:EAX selects all of them.
    unsafe {
        asm!("xsave64 [{}]", in(reg) area, in("eax") u32::MAX, in("edx") u32::MAX, options(nostack, preserves_flags))
    };
}

/// Loads the extended state in `area`.
///
/// # Safety
///
/// `area` must hold an initial or saved XSAVE image.
pub unsafe fn restore_area(area: *const u8) {
    // SAFETY: as for `save_area`; the area holds a valid XSAVE image.
    unsafe {
        asm!("xrstor64 [{}]", in(reg) area, in("eax") u32::MAX, in("edx") u32::MAX, options(nostack, preserves_flags, readonly))
    };
}

impl Drop for ExtendedState {
    fn drop(&mut self) {
        // SAFETY: the area came from `alloc_zeroed` with this layout.
        unsafe { dealloc(self.0.as_ptr(), xsave_layout()) };
    }
}
