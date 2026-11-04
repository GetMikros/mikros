//! The x86_64 state of a kernel thread: its saved stack pointer and
//! extended state, and for a user thread its user segment bases, debug
//! registers and the frames that hold its user registers while it is
//! stopped or in a system call.

use crate::context::{self, ExtendedState};
use crate::percpu;
use crate::sched::{self, Thread};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use kernel_core::arch::ThreadData;
use kernel_core::sync::SpinLock;

const IA32_FS_BASE: u32 = 0xC000_0100;

/// The value DR6 has when no debug condition is recorded.
const DR6_CLEAR: u64 = 0xFFFF_0FF0;

/// Whether DR7 may enable breakpoints on this processor: set when a
/// thread with breakpoints was switched in.
fn breakpoints_loaded() -> &'static AtomicBool {
    &percpu::this().breakpoints_loaded
}

/// The user-mode state of a user thread that is not in its registers
/// while the thread runs in the kernel.
#[derive(Clone, Copy, Default)]
struct UserState {
    /// The entry point, stack pointer and two arguments it starts with.
    start: [u64; 4],
    fs_base: u64,
    gs_base: u64,
}

pub struct ArchThread {
    /// The stack pointer saved while the thread does not run.
    rsp: UnsafeCell<u64>,
    extended: ExtendedState,
    user: SpinLock<UserState>,
    /// The user registers of the thread while it is in state `EXCEPTION`
    /// or `SUSPENDED`, on its kernel stack, or null.
    exception_frame: AtomicPtr<crate::idt::Frame>,
    /// The registers the thread entered its current system call with, on
    /// its kernel stack, or null.
    syscall_frame: AtomicPtr<crate::syscall::Frame>,
    /// Set when the registers of `syscall_frame` may have been changed
    /// while the thread was suspended in the call.
    syscall_changed: AtomicBool,
    /// DR0 to DR3, DR6 and DR7 of a user thread, checked as
    /// `thread_state::parse_debug` does.
    debug: SpinLock<[u64; 6]>,
    /// Set when one of the thread's breakpoints fired in kernel mode, so
    /// that a `DEBUG` exception is reported before it returns to user
    /// mode.
    debug_pending: AtomicBool,
}

// SAFETY: `rsp` is written only by `context_switch` while switching away
// from the thread and read only while switching to it, on the processor
// that runs it, with interrupts disabled.
unsafe impl Sync for ArchThread {}

/// The x86_64 state behind a thread's `ThreadData`.
pub fn of(data: &ThreadData) -> &ArchThread {
    // SAFETY: on x86_64 every `ThreadData` comes from `thread_new`, which
    // makes it from a boxed `ArchThread`, alive as long as `data`.
    unsafe { &*data.as_ptr().cast::<ArchThread>() }
}

impl ArchThread {
    pub fn new() -> Option<ArchThread> {
        Some(ArchThread {
            rsp: UnsafeCell::new(0),
            extended: ExtendedState::new()?,
            user: SpinLock::new(kernel_core::lock_order::THREAD_USER, UserState::default()),
            exception_frame: AtomicPtr::new(core::ptr::null_mut()),
            syscall_frame: AtomicPtr::new(core::ptr::null_mut()),
            syscall_changed: AtomicBool::new(false),
            debug: SpinLock::new(kernel_core::lock_order::THREAD_DEBUG, [0; 6]),
            debug_pending: AtomicBool::new(false),
        })
    }

    /// Where the thread's stack pointer is saved while it does not run.
    pub fn saved_rsp(&self) -> *mut u64 {
        self.rsp.get()
    }

    /// Records where a new user thread starts in user mode: RIP, RSP and
    /// the two arguments.
    pub fn set_start(&self, start: [u64; 4]) {
        self.user.lock().start = start;
    }

    /// Where the user thread starts in user mode.
    pub fn start(&self) -> [u64; 4] {
        self.user.lock().start
    }

    /// Saves the user state that the next thread replaces.
    pub fn switch_out(&self, user: bool) {
        self.extended.save();
        if user {
            let mut state = self.user.lock();
            // SAFETY: both MSRs exist in long mode; in the kernel the user
            // GS base is in IA32_KERNEL_GS_BASE.
            unsafe {
                state.fs_base = crate::cpu::read_msr(IA32_FS_BASE);
                state.gs_base = crate::cpu::read_msr(percpu::IA32_KERNEL_GS_BASE);
            }
            let mut debug = self.debug.lock();
            if debug[5] != 0 {
                debug[4] = crate::cpu::read_dr(6);
            }
        }
    }

    /// Loads the extended state and breakpoints of the thread about to
    /// run, whose kernel stack ends at `top`, and records the stack for
    /// backtraces. For a user thread also loads the stack for entries from
    /// user mode and the FS and GS bases.
    pub fn switch_in(&self, top: u64, user: bool) {
        percpu::this().thread_stack.store(top, Ordering::Relaxed);
        self.extended.restore();
        self.load_breakpoints();
        if user {
            self.switch_in_user(top);
        }
    }

    fn switch_in_user(&self, top: u64) {
        crate::gdt::set_rsp0(top);
        percpu::this().kernel_stack.store(top, Ordering::Relaxed);
        let user = *self.user.lock();
        // SAFETY: the bases are user addresses or zero, and the kernel
        // uses neither FS nor the inactive GS base.
        unsafe {
            crate::cpu::write_msr(IA32_FS_BASE, user.fs_base);
            crate::cpu::write_msr(percpu::IA32_KERNEL_GS_BASE, user.gs_base);
        }
    }

    /// Loads the thread's breakpoints, or disables those of the thread
    /// that ran before.
    fn load_breakpoints(&self) {
        let debug = *self.debug.lock();
        let dr7 = debug[5];
        if dr7 == 0 {
            if breakpoints_loaded().swap(false, Ordering::Relaxed) {
                // SAFETY: zero disables every breakpoint.
                unsafe { crate::cpu::write_dr(7, 0) };
            }
            return;
        }
        // SAFETY: disabled breakpoints get address 0 and enabled ones were
        // checked to be user addresses; DR6 keeps only its lower half; DR7
        // was checked to enable only local, non-I/O breakpoints.
        unsafe {
            crate::cpu::write_dr(7, 0);
            for (i, &addr) in debug[..4].iter().enumerate() {
                let enabled = dr7 >> (2 * i) & 1 != 0;
                crate::cpu::write_dr(i as u8, if enabled { addr } else { 0 });
            }
            crate::cpu::write_dr(6, debug[4] & 0xFFFF_FFFF);
            crate::cpu::write_dr(7, dr7);
        }
        breakpoints_loaded().store(true, Ordering::Relaxed);
    }

    /// Records the user registers of the stopped thread, or null once it
    /// runs on.
    pub fn set_exception_frame(&self, frame: *mut crate::idt::Frame) {
        self.exception_frame.store(frame, Ordering::Release);
    }

    /// Whether the stopped thread's user registers are recorded.
    pub fn has_exception_frame(&self) -> bool {
        !self.exception_frame.load(Ordering::Acquire).is_null()
    }

    /// Calls `f` with the debug registers.
    pub fn with_debug<R>(&self, f: impl FnOnce(&mut [u64; 6]) -> R) -> R {
        f(&mut self.debug.lock())
    }

    /// Records a debug exception that the thread's breakpoints raised in
    /// kernel mode, with DR6 `dr6`, and clears DR6.
    pub fn defer_debug(&self, dr6: u64) {
        self.debug.lock()[4] = dr6;
        // SAFETY: this is DR6's value with no condition recorded.
        unsafe { crate::cpu::write_dr(6, DR6_CLEAR) };
        self.debug_pending.store(true, Ordering::Release);
    }

    /// Whether a debug exception was deferred, clearing the mark.
    pub fn take_deferred_debug(&self) -> bool {
        self.debug_pending.swap(false, Ordering::AcqRel)
    }

    /// Whether the thread has breakpoints enabled.
    pub fn has_breakpoints(&self) -> bool {
        self.debug.lock()[5] != 0
    }

    pub fn set_syscall_frame(&self, frame: *mut crate::syscall::Frame) {
        self.syscall_frame.store(frame, Ordering::Release);
    }

    pub fn syscall_frame(&self) -> *mut crate::syscall::Frame {
        self.syscall_frame.load(Ordering::Acquire)
    }

    pub fn mark_syscall_changed(&self) {
        self.syscall_changed.store(true, Ordering::Release);
    }

    pub fn take_syscall_changed(&self) -> bool {
        self.syscall_changed.swap(false, Ordering::AcqRel)
    }

    pub fn extended_state(&self) -> &context::ExtendedState {
        &self.extended
    }

    /// Calls `f` with the recorded user registers, the user FS and GS
    /// bases and the extended state of the stopped thread, which does not
    /// run meanwhile.
    pub fn with_stopped_state<R>(
        &self,
        f: impl FnOnce(&mut crate::idt::Frame, &mut [u64; 2], &context::ExtendedState) -> R,
    ) -> R {
        let frame = self.exception_frame.load(Ordering::Acquire);
        let mut user = self.user.lock();
        let mut bases = [user.fs_base, user.gs_base];
        // SAFETY: the caller checked the frame is recorded; it is on the
        // thread's kernel stack, which stays until the thread runs again,
        // and the thread is not running.
        let result = f(unsafe { &mut *frame }, &mut bases, &self.extended);
        [user.fs_base, user.gs_base] = bases;
        result
    }
}

/// The x86_64 operations on a kernel thread.
pub trait ThreadX86 {
    /// The thread's x86_64 state.
    fn x86(&self) -> &ArchThread;

    /// Whether the thread is in state `EXCEPTION` or `SUSPENDED` with its
    /// user registers recorded.
    fn stopped(&self) -> bool;

    /// Calls `f` with the debug registers of a stopped thread, or returns
    /// `None` when it is not stopped.
    fn with_stopped_debug<R>(&self, f: impl FnOnce(&mut [u64; 6]) -> R) -> Option<R> {
        self.stopped().then(|| self.x86().with_debug(f))
    }

    /// Calls `f` with the user registers, the user FS and GS bases and the
    /// extended state of a stopped thread, or returns `None` when it is
    /// not stopped. The thread does not run meanwhile.
    fn with_stopped_state<R>(
        &self,
        f: impl FnOnce(&mut crate::idt::Frame, &mut [u64; 2], &context::ExtendedState) -> R,
    ) -> Option<R> {
        self.stopped().then(|| self.x86().with_stopped_state(f))
    }
}

impl ThreadX86 for Thread {
    fn x86(&self) -> &ArchThread {
        of(self.arch())
    }

    fn stopped(&self) -> bool {
        matches!(self.state(), sched::EXCEPTION | sched::SUSPENDED)
            && self.x86().has_exception_frame()
    }
}

/// The user FS and GS bases of the running thread.
pub fn user_bases() -> (u64, u64) {
    // SAFETY: both MSRs exist in long mode; in the kernel the user GS
    // base is in IA32_KERNEL_GS_BASE.
    unsafe {
        (
            crate::cpu::read_msr(IA32_FS_BASE),
            crate::cpu::read_msr(percpu::IA32_KERNEL_GS_BASE),
        )
    }
}

/// A user segment base of a thread.
pub enum UserBase {
    Fs,
    Gs,
}

/// Sets a user segment base of the calling user thread. Its current
/// values are in the MSRs until `ArchThread::switch_out` saves them.
pub fn set_user_base(base: UserBase, value: u64) {
    let msr = match base {
        UserBase::Fs => IA32_FS_BASE,
        // In the kernel the user GS base is in IA32_KERNEL_GS_BASE.
        UserBase::Gs => percpu::IA32_KERNEL_GS_BASE,
    };
    // SAFETY: both MSRs exist in long mode, and the kernel uses neither
    // for itself while it runs.
    unsafe { crate::cpu::write_msr(msr, value) };
}

/// The first code of a user thread, on its kernel stack.
pub fn user_entry(_: usize) {
    let Some(thread) = sched::current() else {
        unreachable!("a user thread started without being current");
    };
    let [rip, rsp, arg1, arg2] = thread.x86().start();
    let report = thread.process().is_some_and(|p| p.debug_events());
    if !report && !thread.suspend_pending() {
        drop(thread);
        sched::exit_if_dead();
        // SAFETY: `switch_in` loaded the thread's address space, extended
        // state and user bases before switching to it.
        unsafe { context::enter_user(rip, rsp, arg1, arg2) }
    }
    let mut frame = crate::idt::Frame {
        rdi: arg1,
        rsi: arg2,
        rip,
        cs: u64::from(crate::gdt::USER_CODE) | 3,
        rflags: 0x202,
        rsp,
        ss: u64::from(crate::gdt::USER_DATA) | 3,
        ..Default::default()
    };
    if report {
        crate::exception::report_event(
            &mut frame,
            kernel_core::exception::kind::THREAD_STARTING,
            rip,
        );
    }
    crate::exception::stay_suspended(&mut frame, &thread);
    drop(thread);
    sched::exit_if_dead();
    // SAFETY: as above; the handler could change only what
    // `thread_write_state` allows.
    unsafe { context::enter_user_frame(&frame) }
}
