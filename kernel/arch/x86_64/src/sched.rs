//! Kernel threads and the scheduler of this processor.
//!
//! Kernel code runs with interrupts disabled. They are enabled only while
//! the processor idles and at preemption points, so the scheduler's state
//! is never touched from an interrupt that arrives in the middle of a
//! change to it.

use crate::context::{self, ExtendedState};
use crate::percpu;
use crate::process::Process;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use kernel_core::deadline::{Deadlines, INFINITE};
use kernel_core::object::{Header, Name, Object, ObjectType, signals};
use kernel_core::sched::{DEFAULT_PRIORITY, Entity, Fair};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

const IA32_FS_BASE: u32 = 0xC000_0100;

/// The size of a kernel thread's stack.
pub const KERNEL_STACK_SIZE: u64 = 32 << 10;

pub const NEW: u32 = 0;
pub const RUNNABLE: u32 = 1;
pub const BLOCKED: u32 = 2;
pub const EXCEPTION: u32 = 3;
pub const DEAD: u32 = 4;
pub const SUSPENDED: u32 = 5;

/// Why a thread left state `EXCEPTION`: resumed by its handler, or woken
/// because the handler's endpoint was closed.
pub const RESUMED: u32 = 1;
pub const HANDLER_GONE: u32 = 2;
/// Resumed with `DELIVER_SIGNAL`.
pub const DELIVER_SIGNAL: u32 = 3;

pub struct Thread {
    header: Header,
    #[allow(
        dead_code,
        reason = "names are only for diagnostics, which nothing prints yet"
    )]
    name: Name,
    /// The process of a user thread.
    process: Option<Arc<Process>>,
    /// Kernel memory charged to the process's budget for the thread.
    charged: u64,
    state: AtomicU32,
    /// The stack pointer saved while the thread does not run.
    rsp: UnsafeCell<u64>,
    extended: ExtendedState,
    /// The top of the kernel stack, or 0 once it is freed.
    stack_top: AtomicU64,
    /// The coverage buffer of the thread in the fuzz build.
    #[cfg(feature = "fuzz")]
    coverage: SpinLock<Option<crate::coverage::Buffer>>,
    #[cfg(feature = "fuzz")]
    alloc_failures: crate::fuzz::AllocFailures,
    entity: SpinLock<Entity>,
    /// The thread's priority, from 0 to `MAX_PRIORITY`.
    priority: AtomicU32,
    /// Nanoseconds the thread has run.
    cpu_time: AtomicI64,
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
    /// The number of `thread_suspend` calls not yet undone.
    suspend_count: AtomicU64,
    /// Set when a wait ended because the thread is to be suspended, so
    /// that its system call runs again.
    restart: AtomicBool,
    /// `RESUMED` or `HANDLER_GONE` once the thread leaves `EXCEPTION`.
    woken: AtomicU32,
    /// DR0 to DR3, DR6 and DR7 of a user thread, checked as
    /// `thread_state::parse_debug` does.
    debug: SpinLock<[u64; 6]>,
    /// Set when one of the thread's breakpoints fired in kernel mode, so
    /// that a `DEBUG` exception is reported before it returns to user
    /// mode.
    debug_pending: AtomicBool,
    /// The type of the exception the thread is or was last in.
    exception_kind: AtomicU32,
    /// The identifier of the thread's latest exception report, 0 before
    /// its first.
    report_id: AtomicU64,
    /// Set when the thread is to end, after a `THREAD_EXITING` report,
    /// before it returns to user mode.
    exit_requested: AtomicBool,
    signals: SpinLock<crate::signal::ThreadSignals>,
    /// The context address `signal_return` was called with, or 0.
    sigreturn: AtomicU64,
    /// Set while the thread waits in `signal_wait`.
    waiting_for_signals: AtomicBool,
    /// The number of the system call that last ended with
    /// `ERR_INTERRUPTED`, until signals are next delivered, or 0.
    interrupted: AtomicU64,
}

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

// SAFETY: `rsp` is written only by `context_switch` while switching away
// from the thread and read only while switching to it, on the processor
// that runs it, with interrupts disabled.
unsafe impl Sync for Thread {}

impl Thread {
    /// A thread with a new kernel stack, in `state`, not queued.
    fn new(
        process: Option<Arc<Process>>,
        name: Name,
        state: u32,
        charged: u64,
    ) -> Result<Arc<Thread>, Error> {
        let extended = ExtendedState::new().ok_or(Error::NoMemory)?;
        let stack = crate::space::alloc_pages(KERNEL_STACK_SIZE).ok_or(Error::NoMemory)?;
        // Every thread that can run has room in the run queue from when
        // it was created.
        if this_cpu().lock().fair.try_reserve(1).is_err() {
            crate::space::free_pages(stack, KERNEL_STACK_SIZE);
            return Err(Error::NoMemory);
        }
        // The thread frees its stack and charge when it goes, also on
        // failure.
        kernel_core::fallible::try_arc(Thread {
            header: Header::new(ObjectType::Thread),
            name,
            process,
            charged,
            state: AtomicU32::new(state),
            rsp: UnsafeCell::new(0),
            extended,
            stack_top: AtomicU64::new(stack + KERNEL_STACK_SIZE),
            #[cfg(feature = "fuzz")]
            coverage: SpinLock::new(None),
            #[cfg(feature = "fuzz")]
            alloc_failures: Default::default(),
            entity: SpinLock::new(Entity::new(DEFAULT_PRIORITY)),
            priority: AtomicU32::new(DEFAULT_PRIORITY),
            cpu_time: AtomicI64::new(0),
            user: SpinLock::new(UserState::default()),
            exception_frame: AtomicPtr::new(core::ptr::null_mut()),
            syscall_frame: AtomicPtr::new(core::ptr::null_mut()),
            syscall_changed: AtomicBool::new(false),
            suspend_count: AtomicU64::new(0),
            restart: AtomicBool::new(false),
            woken: AtomicU32::new(0),
            debug: SpinLock::new([0; 6]),
            debug_pending: AtomicBool::new(false),
            exception_kind: AtomicU32::new(0),
            report_id: AtomicU64::new(0),
            exit_requested: AtomicBool::new(false),
            signals: SpinLock::new(crate::signal::ThreadSignals::default()),
            sigreturn: AtomicU64::new(0),
            waiting_for_signals: AtomicBool::new(false),
            interrupted: AtomicU64::new(0),
        })
    }

    /// A user thread of `process` in state `NEW`. Its memory is charged
    /// to the process's budget when `charge` is set.
    pub fn new_user(process: Arc<Process>, name: Name, charge: bool) -> Result<Arc<Thread>, Error> {
        let charged = if charge {
            // Its entries in the process's thread list and stop list,
            // the run queue and the sleepers count twice, for their share
            // of storage.
            let entries = size_of::<alloc::sync::Weak<Thread>>()
                + size_of::<Arc<Thread>>()
                + Fair::<Queued>::ENTRY_SIZE
                + Deadlines::<Queued>::ENTRY_SIZE;
            let size = size_of::<Thread>() as u64
                + KERNEL_STACK_SIZE
                + context::extended_state_size() as u64
                + 2 * entries as u64;
            process.budget().charge_kernel(size)?;
            size
        } else {
            0
        };
        let budget = process.budget().clone();
        Thread::new(Some(process), name, NEW, charged).inspect_err(|_| {
            budget.release_kernel(charged);
        })
    }

    #[allow(
        dead_code,
        reason = "names are only for diagnostics, which nothing prints yet"
    )]
    pub fn name(&self) -> &Name {
        &self.name
    }

    pub fn process(&self) -> Option<&Arc<Process>> {
        self.process.as_ref()
    }

    pub fn state(&self) -> u32 {
        self.state.load(Ordering::Acquire)
    }

    /// Starts a `NEW` user thread in user mode at `rip` with stack
    /// pointer `rsp` and the two arguments, and queues it. The caller has
    /// checked the thread's and its process's state.
    pub fn start_user(thread: &Arc<Thread>, rip: u64, rsp: u64, arg1: u64, arg2: u64) {
        debug_assert!(thread.process.is_some() && thread.state() == NEW);
        thread.user.lock().start = [rip, rsp, arg1, arg2];
        let top = thread.stack_top.load(Ordering::Relaxed);
        // SAFETY: a NEW thread has never run, so nothing uses its stack
        // or reads its saved pointer.
        unsafe { *thread.rsp.get() = context::prepare(top, user_entry as *const () as u64, 0) };
        thread.state.store(RUNNABLE, Ordering::Release);
        let mut cpu = this_cpu().lock();
        let entity = *thread.entity.lock();
        cpu.enqueue(Queued::new(thread.clone()), entity);
    }

    /// Saves the user state that the next thread replaces.
    fn switch_out(&self) {
        self.extended.save();
        if self.process.is_some() {
            let mut user = self.user.lock();
            // SAFETY: both MSRs exist in long mode; in the kernel the user
            // GS base is in IA32_KERNEL_GS_BASE.
            unsafe {
                user.fs_base = crate::cpu::read_msr(IA32_FS_BASE);
                user.gs_base = crate::cpu::read_msr(percpu::IA32_KERNEL_GS_BASE);
            }
            let mut debug = self.debug.lock();
            if debug[5] != 0 {
                debug[4] = crate::cpu::read_dr(6);
            }
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

    /// Whether the thread is in state `EXCEPTION` or `SUSPENDED` with its
    /// user registers available.
    fn stopped(&self) -> bool {
        matches!(self.state(), EXCEPTION | SUSPENDED)
            && !self.exception_frame.load(Ordering::Acquire).is_null()
    }

    /// Calls `f` with the debug registers of a thread in state
    /// `EXCEPTION` or `SUSPENDED`, or returns `None` when it is in another
    /// state.
    pub fn with_stopped_debug<R>(&self, f: impl FnOnce(&mut [u64; 6]) -> R) -> Option<R> {
        if !self.stopped() {
            return None;
        }
        Some(f(&mut self.debug.lock()))
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

    /// Makes the thread restore the signal context at `context` before
    /// it returns to user mode.
    pub fn request_sigreturn(&self, context: u64) {
        self.sigreturn.store(context, Ordering::Release);
    }

    /// The context `request_sigreturn` stored, clearing it.
    pub fn take_sigreturn(&self) -> Option<u64> {
        Some(self.sigreturn.swap(0, Ordering::AcqRel)).filter(|&c| c != 0)
    }

    /// Records that system call `number` ended with `ERR_INTERRUPTED`.
    pub fn set_interrupted(&self, number: u64) {
        self.interrupted.store(number, Ordering::Release);
    }

    /// The system call that ended with `ERR_INTERRUPTED`, clearing it.
    pub fn take_interrupted(&self) -> Option<u64> {
        Some(self.interrupted.swap(0, Ordering::AcqRel)).filter(|&n| n != 0)
    }

    /// Whether the thread waits in `signal_wait`.
    pub fn waits_for_signals(&self) -> bool {
        self.waiting_for_signals.load(Ordering::Acquire)
    }

    pub fn extended_state(&self) -> &context::ExtendedState {
        &self.extended
    }

    pub fn signals(&self) -> &SpinLock<crate::signal::ThreadSignals> {
        &self.signals
    }

    /// The type of the exception the thread is in.
    pub fn exception_kind(&self) -> u32 {
        self.exception_kind.load(Ordering::Acquire)
    }

    /// Takes the identifier of a new exception report, greater than that
    /// of every earlier one.
    pub fn next_report_id(&self) -> u64 {
        self.report_id.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// The identifier of the thread's latest exception report.
    pub fn report_id(&self) -> u64 {
        self.report_id.load(Ordering::Acquire)
    }

    /// Makes the thread end before it next returns to user mode, after
    /// reporting it.
    pub fn request_exit(&self) {
        self.exit_requested.store(true, Ordering::Release);
    }

    /// Whether the thread is to end, leaving the request.
    pub fn exit_requested(&self) -> bool {
        self.exit_requested.load(Ordering::Acquire)
    }

    /// Whether the thread is to end, clearing the request.
    pub fn take_exit_request(&self) -> bool {
        self.exit_requested.swap(false, Ordering::AcqRel)
    }

    /// Increases the suspend count. Fails with `BadState` when the thread
    /// is `NEW` or `DEAD`.
    pub fn suspend(&self) -> Result<(), Error> {
        if matches!(self.state(), NEW | DEAD) {
            return Err(Error::BadState);
        }
        self.suspend_count.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Decreases the suspend count and returns whether it reached zero.
    /// Fails with `BadState` when the thread is `NEW` or `DEAD` or the
    /// count is zero.
    pub fn resume(&self) -> Result<bool, Error> {
        if matches!(self.state(), NEW | DEAD) {
            return Err(Error::BadState);
        }
        // The count is decreased in one step, so that two resumes never
        // take the same count.
        let count = self
            .suspend_count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |c| c.checked_sub(1))
            .map_err(|_| Error::BadState)?;
        Ok(count == 1)
    }

    /// Whether the thread is to be suspended.
    pub fn suspend_pending(&self) -> bool {
        self.suspend_count.load(Ordering::Acquire) != 0
    }

    /// Whether a wait in a system call must end because the thread is to
    /// be suspended; the call then runs again once it is resumed.
    pub fn leave_wait_to_suspend(&self) -> bool {
        let leave = self.suspend_pending();
        if leave {
            self.restart.store(true, Ordering::Release);
        }
        leave
    }

    /// Whether the system call that just ended is to run again, clearing
    /// the mark.
    pub fn take_restart(&self) -> bool {
        self.restart.swap(false, Ordering::AcqRel)
    }

    /// Records the registers the thread entered a system call with, or
    /// null once it leaves the call.
    pub fn set_syscall_frame(&self, frame: *mut crate::syscall::Frame) {
        self.syscall_frame.store(frame, Ordering::Release);
    }

    /// The registers of the system call the thread is in, or null.
    pub fn syscall_frame(&self) -> *mut crate::syscall::Frame {
        self.syscall_frame.load(Ordering::Acquire)
    }

    /// Records that the registers of the system call may have changed.
    pub fn mark_syscall_changed(&self) {
        self.syscall_changed.store(true, Ordering::Release);
    }

    /// Whether the registers of the system call may have changed,
    /// clearing the mark.
    pub fn take_syscall_changed(&self) -> bool {
        self.syscall_changed.swap(false, Ordering::AcqRel)
    }

    /// Whether the thread has breakpoints enabled.
    pub fn has_breakpoints(&self) -> bool {
        self.debug.lock()[5] != 0
    }

    /// Loads the user state of the thread about to run: its extended
    /// state, and for a user thread its address space, its kernel stack
    /// for entries from user mode, and its FS and GS bases.
    /// The thread's coverage buffer.
    #[cfg(feature = "fuzz")]
    pub fn coverage(&self) -> &SpinLock<Option<crate::coverage::Buffer>> {
        &self.coverage
    }

    /// The thread's allocations that are to fail.
    #[cfg(feature = "fuzz")]
    pub fn alloc_failures(&self) -> &crate::fuzz::AllocFailures {
        &self.alloc_failures
    }

    fn switch_in(&self) {
        let top = self.stack_top.load(Ordering::Relaxed);
        percpu::this().thread_stack.store(top, Ordering::Relaxed);
        self.extended.restore();
        self.load_breakpoints();
        crate::process::switch_ports(self.process.as_deref());
        #[cfg(feature = "fuzz")]
        crate::coverage::switch_in(self);
        #[cfg(feature = "fuzz")]
        crate::fuzz::switch_in(Some((self.header.koid(), &self.alloc_failures)));
        let Some(process) = &self.process else {
            return;
        };
        process.aspace().activate();
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

    /// Calls `f` with the user registers, the user FS and GS bases and the
    /// extended state of a thread in state `EXCEPTION` or `SUSPENDED`, or
    /// returns `None` when it is in another state. The thread does not
    /// run meanwhile.
    pub fn with_stopped_state<R>(
        &self,
        f: impl FnOnce(&mut crate::idt::Frame, &mut [u64; 2], &context::ExtendedState) -> R,
    ) -> Option<R> {
        if !self.stopped() {
            return None;
        }
        let frame = self.exception_frame.load(Ordering::Acquire);
        let mut user = self.user.lock();
        let mut bases = [user.fs_base, user.gs_base];
        // SAFETY: the frame is on the thread's kernel stack, which stays
        // until the thread runs again, and the thread is not running.
        let result = f(unsafe { &mut *frame }, &mut bases, &self.extended);
        [user.fs_base, user.gs_base] = bases;
        Some(result)
    }

    /// The nanoseconds the thread has run, including the slice it is
    /// running now.
    pub fn cpu_time(&self) -> i64 {
        self.cpu_time.load(Ordering::Relaxed) + running_slice(|t| core::ptr::eq(t, self))
    }

    pub fn priority(&self) -> u32 {
        self.priority.load(Ordering::Relaxed)
    }

    /// The `THREAD` information record.
    pub fn info(&self) -> [u8; INFO_THREAD_SIZE] {
        let state = self.state();
        let exception = if state == EXCEPTION {
            self.exception_kind()
        } else {
            0
        };
        let mut record = [0u8; INFO_THREAD_SIZE];
        record[0x00..0x04].copy_from_slice(&state.to_le_bytes());
        record[0x04..0x08].copy_from_slice(&self.priority().to_le_bytes());
        record[0x08..0x0C].copy_from_slice(&exception.to_le_bytes());
        record[0x10..0x18].copy_from_slice(&self.cpu_time().to_le_bytes());
        record
    }
}

impl Object for Thread {
    fn header(&self) -> &Header {
        &self.header
    }

    fn related_koid(&self) -> u64 {
        self.process.as_ref().map_or(0, |p| p.header().koid())
    }
}

impl Drop for Thread {
    fn drop(&mut self) {
        free_stack(self);
        if let Some(process) = &self.process {
            process.budget().release_kernel(self.charged);
        }
    }
}

/// A queued thread, holding one reference to it.
#[derive(Clone, Copy, PartialEq)]
struct Queued(*const Thread);

impl Queued {
    fn new(thread: Arc<Thread>) -> Queued {
        Queued(Arc::into_raw(thread))
    }

    fn take(self) -> Arc<Thread> {
        // SAFETY: the pointer came from `Arc::into_raw` in `new`, and each
        // queued value is taken once.
        unsafe { Arc::from_raw(self.0) }
    }
}

/// The scheduler state of one processor.
pub struct CpuSched {
    fair: Fair<Queued>,
    /// The running thread; `None` while the processor idles.
    current: Option<Arc<Thread>>,
    /// The idle loop's stack pointer while a thread runs.
    idle_rsp: u64,
    /// When the running thread started its slice, and the slice's end.
    slice_start: i64,
    slice_end: i64,
    /// A thread that exited, whose stack is freed after switching away.
    dead: Option<Arc<Thread>>,
    /// Threads sleeping until a deadline.
    sleepers: Deadlines<Queued>,
    /// Armed `Timer` objects.
    timers: Deadlines<crate::ktimer::Armed>,
    /// The kernel's own deadlines: sampling a clock counter that wraps,
    /// and reseeding the random number generator, or `INFINITE`.
    clock_due: i64,
    reseed_due: i64,
}

// SAFETY: queued pointers are references to threads, which are Sync; the
// state is used only by its own processor.
unsafe impl Send for CpuSched {}

impl CpuSched {
    pub const fn new() -> CpuSched {
        CpuSched {
            fair: Fair::new(),
            current: None,
            idle_rsp: 0,
            slice_start: 0,
            slice_end: 0,
            dead: None,
            sleepers: Deadlines::new(),
            timers: Deadlines::new(),
            clock_due: INFINITE,
            reseed_due: INFINITE,
        }
    }

    /// Queues `thread` with `entity`, and marks a reschedule when the
    /// queue would pick it before the running thread.
    fn enqueue(&mut self, thread: Queued, entity: Entity) {
        if self.fair.enqueue(thread, entity) {
            reschedule().store(true, Ordering::Relaxed);
        }
    }
}

fn this_cpu() -> &'static SpinLock<CpuSched> {
    &percpu::this().sched
}

/// Set when the thread running on this processor is to give way: by the
/// timer interrupt when its slice has ended, and when a thread is queued
/// that the queue would pick before it.
fn reschedule() -> &'static AtomicBool {
    &percpu::this().reschedule
}

/// Creates a kernel thread that runs `entry(arg)` and queues it.
#[allow(dead_code, reason = "nothing starts kernel threads yet")]
pub fn spawn(entry: fn(usize), arg: usize) -> Result<Arc<Thread>, Error> {
    let thread = Thread::new(None, Name::EMPTY, RUNNABLE, 0)?;
    let top = thread.stack_top.load(Ordering::Relaxed);
    // SAFETY: the stack was just mapped and nothing uses it; the thread
    // is not queued yet, so nothing reads its saved pointer.
    unsafe { *thread.rsp.get() = context::prepare(top, entry as usize as u64, arg as u64) };
    let mut cpu = this_cpu().lock();
    let entity = *thread.entity.lock();
    cpu.enqueue(Queued::new(thread.clone()), entity);
    Ok(thread)
}

/// Frees the thread's kernel stack, unless it is already freed. Nothing
/// may run on it.
fn free_stack(thread: &Thread) {
    let top = thread.stack_top.swap(0, Ordering::AcqRel);
    if top != 0 {
        crate::space::free_pages(top - KERNEL_STACK_SIZE, KERNEL_STACK_SIZE);
    }
}

/// Frees what the thread that ran before left behind.
fn finish_switch() {
    let dead = this_cpu().lock().dead.take();
    if let Some(dead) = dead {
        free_stack(&dead);
    }
}

/// Stops the running thread, queuing it again if `requeue`, and runs the
/// next thread, or idles. Returns when the thread runs again, unless it
/// is not queued and nothing wakes it.
fn schedule(requeue: bool) {
    // RFLAGS.AC is not switched, so a thread that kept it would let the
    // next one reach user pages past SMAP.
    debug_assert!(
        crate::cpu::rflags() & crate::cpu::RFLAGS_AC == 0,
        "scheduling with RFLAGS.AC set"
    );
    debug_assert!(
        !crate::idt::handling(),
        "scheduling while handling an interrupt"
    );
    let now = crate::time::now();
    let mut cpu = this_cpu().lock();
    let Some(current) = cpu.current.take() else {
        return;
    };
    let ran = now - cpu.slice_start;
    current.cpu_time.fetch_add(ran, Ordering::Relaxed);
    if let Some(process) = &current.process {
        process.add_cpu_time(ran);
    }
    cpu.fair.charge(ran);
    // A thread made DEAD by another goes on running until it reaches
    // `exit_if_dead`, so it is queued again like a runnable one.
    if requeue && current.state() != BLOCKED {
        // Room for the running thread was reserved when it was queued.
        cpu.fair.requeue(Queued::new(current.clone()));
    } else if let Some(stopped) = cpu.fair.stop() {
        // It keeps its lag for when it is runnable again. One woken
        // before it got here was queued again instead.
        *current.entity.lock() = stopped;
    }
    let picked = cpu.fair.pick();
    if let Some((next, entity)) = picked
        && next.0 == Arc::as_ptr(&current)
    {
        // The running thread goes on.
        *current.entity.lock() = entity;
        drop(next.take());
        start_slice(&mut cpu, now, current);
        return;
    }
    let save = current.rsp.get();
    current.switch_out();
    let load = match picked {
        Some((next, entity)) => {
            let next = next.take();
            *next.entity.lock() = entity;
            next.switch_in();
            // SAFETY: `next` is not running, so its saved pointer is
            // stable.
            let rsp = unsafe { *next.rsp.get() };
            start_slice(&mut cpu, now, next);
            rsp
        }
        None => {
            #[cfg(feature = "fuzz")]
            {
                crate::fuzz::switch_in(None);
                // The idle loop runs on behalf of no thread.
                crate::coverage::idle();
            }
            cpu.idle_rsp
        }
    };
    if !requeue && current.state() == DEAD {
        cpu.dead = Some(current.clone());
    }
    drop(cpu);
    drop(current);
    // SAFETY: `load` is a thread's saved stack or the idle loop's, and
    // `save` belongs to a thread that the scheduler still references.
    unsafe { context::switch(save, load) };
    finish_switch();
}

/// Makes `thread` the running thread, starting its slice, and arms the
/// timer for it. A reschedule marked for the thread that ran before is
/// dropped.
fn start_slice(cpu: &mut CpuSched, now: i64, thread: Arc<Thread>) {
    reschedule().store(false, Ordering::Relaxed);
    cpu.current = Some(thread);
    cpu.slice_start = now;
    cpu.slice_end = now.saturating_add(cpu.fair.slice());
    arm(cpu);
}

/// Arms the timer for the earlier of the running thread's slice end and
/// the nearest deadline, or stops it when there is neither.
fn arm(cpu: &CpuSched) {
    // Once a reschedule is pending, the ended slice must not interrupt
    // again: in a preempt point that would interrupt at once, forever,
    // before interrupts are disabled again. The next slice arms anew.
    let slice_end = if cpu.current.is_some() && !reschedule().load(Ordering::Relaxed) {
        cpu.slice_end
    } else {
        INFINITE
    };
    let at = [cpu.sleepers.next_finite(), cpu.timers.next_finite()]
        .into_iter()
        .flatten()
        .chain([cpu.clock_due, cpu.reseed_due])
        .fold(slice_end, i64::min);
    if at == INFINITE {
        crate::timer::disarm();
    } else {
        crate::timer::arm(at);
    }
}

/// Blocks the running thread until the monotonic clock reaches
/// `deadline`. Returns at once when it already has.
pub fn sleep_until(deadline: i64) -> Result<(), Error> {
    if deadline <= crate::time::now() {
        return Ok(());
    }
    {
        let mut cpu = this_cpu().lock();
        let Some(current) = cpu.current.clone() else {
            return Ok(());
        };
        if current.state() == DEAD {
            return Ok(());
        }
        cpu.sleepers
            .insert(deadline, Queued::new(current.clone()))
            .map_err(|_| Error::NoMemory)?;
        current.state.store(BLOCKED, Ordering::Release);
    }
    schedule(false);
    Ok(())
}

/// Blocks the running thread in `signal_wait` until `deadline` or until
/// `wake_waiter` wakes it. Fails with `Interrupted` at once, or once
/// woken, while the thread is to end, so that it leaves the wait and
/// reports its exit on the way back to user mode.
pub fn wait_for_signals(thread: &Thread, deadline: i64) -> Result<(), Error> {
    if thread.exit_requested() {
        return Err(Error::Interrupted);
    }
    thread.waiting_for_signals.store(true, Ordering::Release);
    let result = sleep_until(deadline);
    thread.waiting_for_signals.store(false, Ordering::Release);
    match thread.exit_requested() {
        true => Err(Error::Interrupted),
        false => result,
    }
}

/// Wakes `thread` before its deadline if it sleeps.
pub fn wake_sleeper(thread: &Arc<Thread>) {
    let mut cpu = this_cpu().lock();
    let queued = Queued(Arc::as_ptr(thread));
    if thread.state() == BLOCKED && cpu.sleepers.remove(&queued) {
        thread.state.store(RUNNABLE, Ordering::Release);
        let entity = *thread.entity.lock();
        cpu.enqueue(queued, entity);
    }
}

/// Gives up the processor to the next queued thread, if any.
#[allow(dead_code, reason = "nothing starts kernel threads yet")]
pub fn yield_now() {
    schedule(true);
}

/// Lets pending interrupts in, and switches threads if the running
/// thread's slice has ended. No lock may be held.
pub fn preempt_point() {
    debug_assert!(
        !crate::idt::handling(),
        "a preemption point while handling an interrupt"
    );
    debug_assert!(
        kernel_core::sync::none_held(),
        "a preemption point with a lock held"
    );
    // SAFETY: enabling interrupts for one instruction lets the processor
    // take pending ones; no lock is held here.
    unsafe { core::arch::asm!("sti", "nop", "cli", options(nomem, nostack)) };
    if reschedule().swap(false, Ordering::Relaxed) {
        schedule(true);
    }
}

/// Calls `preempt_point` when no lock is held and no interrupt or
/// exception is being handled, for work that may run either way, such as
/// destroying an object.
pub fn preempt_point_if_unlocked() {
    if kernel_core::sync::none_held() && !crate::idt::handling() {
        preempt_point();
    }
}

/// Ends the running thread.
pub fn exit() -> ! {
    if let Some(current) = this_cpu().lock().current.as_ref() {
        current.state.store(DEAD, Ordering::Release);
    }
    schedule(false);
    unreachable!("a dead thread ran again");
}

/// Makes `thread` DEAD and returns the state it was in. A sleeping
/// thread is queued again, so that it runs to `exit_if_dead`; a queued or
/// running one gets there by itself, and a `NEW` one never runs.
pub fn make_dead(thread: &Arc<Thread>) -> u32 {
    let was = thread.state.swap(DEAD, Ordering::AcqRel);
    if was == BLOCKED {
        let mut cpu = this_cpu().lock();
        let queued = Queued(Arc::as_ptr(thread));
        // The reference the sleeper held moves to the run queue. A thread
        // blocked by `block` is not a sleeper and is queued afresh.
        let entity = *thread.entity.lock();
        if cpu.sleepers.remove(&queued) {
            cpu.enqueue(queued, entity);
        } else {
            cpu.enqueue(Queued::new(thread.clone()), entity);
        }
    } else if was == EXCEPTION || was == SUSPENDED {
        queue(thread);
    }
    was
}

/// Puts the running user `thread`, whose user registers are in `frame`,
/// in state `SUSPENDED` while its suspend count is above zero, until
/// `wake_suspended` or `make_dead`. Returns whether it was suspended; it
/// is not while it is to end.
pub fn suspend(thread: &Arc<Thread>, frame: &mut crate::idt::Frame) -> bool {
    if !thread.suspend_pending() || thread.exit_requested() {
        return false;
    }
    thread.exception_frame.store(frame, Ordering::Release);
    let suspended = thread
        .state
        .compare_exchange(RUNNABLE, SUSPENDED, Ordering::AcqRel, Ordering::Acquire)
        .is_ok();
    if suspended {
        thread.header.update_signals(0, signals::SUSPENDED);
        schedule(false);
        // A thread made DEAD was not woken by `wake_suspended`.
        thread.header.update_signals(signals::SUSPENDED, 0);
    }
    thread
        .exception_frame
        .store(core::ptr::null_mut(), Ordering::Release);
    suspended
}

/// Makes `thread` RUNNABLE and queues it if it is in state `SUSPENDED`.
pub fn wake_suspended(thread: &Arc<Thread>) {
    if thread
        .state
        .compare_exchange(SUSPENDED, RUNNABLE, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        thread.header.update_signals(signals::SUSPENDED, 0);
        queue(thread);
    }
}

/// Blocks the running `thread` until `wake` or `make_dead`, calling
/// `release` once it is marked BLOCKED, so that whoever wakes it can
/// only see it blocked. Returns at once when it is not RUNNABLE.
pub fn block(thread: &Arc<Thread>, release: impl FnOnce()) {
    let blocked = thread
        .state
        .compare_exchange(RUNNABLE, BLOCKED, Ordering::AcqRel, Ordering::Acquire)
        .is_ok();
    release();
    if blocked {
        schedule(false);
    }
}

/// Makes a thread that `block` blocked RUNNABLE and queues it.
pub fn wake(thread: &Arc<Thread>) {
    if thread
        .state
        .compare_exchange(BLOCKED, RUNNABLE, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        queue(thread);
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

/// Queues `thread`, which is not running and not queued.
fn queue(thread: &Arc<Thread>) {
    let entity = *thread.entity.lock();
    this_cpu()
        .lock()
        .enqueue(Queued::new(thread.clone()), entity);
}

/// Puts the running user thread, whose user registers are in `frame`,
/// in state `EXCEPTION`. It goes on running until `wait_exception`.
/// Returns false, changing nothing, when it is not RUNNABLE.
pub fn enter_exception(thread: &Thread, frame: &mut crate::idt::Frame, kind: u32) -> bool {
    thread.exception_frame.store(frame, Ordering::Release);
    thread.woken.store(0, Ordering::Relaxed);
    thread.exception_kind.store(kind, Ordering::Release);
    let entered = thread
        .state
        .compare_exchange(RUNNABLE, EXCEPTION, Ordering::AcqRel, Ordering::Acquire)
        .is_ok();
    if !entered {
        thread
            .exception_frame
            .store(core::ptr::null_mut(), Ordering::Release);
    }
    entered
}

/// Takes the running thread out of state `EXCEPTION` before it waited.
pub fn leave_exception(thread: &Thread) {
    thread
        .exception_frame
        .store(core::ptr::null_mut(), Ordering::Release);
    let _ = thread
        .state
        .compare_exchange(EXCEPTION, RUNNABLE, Ordering::AcqRel, Ordering::Acquire);
}

/// Runs other threads until the running thread, which `enter_exception`
/// put in state `EXCEPTION`, is resumed or made DEAD. Returns why it left
/// the state, or 0 when it was made DEAD.
pub fn wait_exception(thread: &Thread) -> u32 {
    // Nothing else runs on this processor before the thread switches
    // out, so nothing can have queued it yet.
    if thread.state() == EXCEPTION {
        schedule(false);
    }
    thread
        .exception_frame
        .store(core::ptr::null_mut(), Ordering::Release);
    thread.woken.load(Ordering::Acquire)
}

/// Makes `thread` RUNNABLE and queues it if it is in state `EXCEPTION`,
/// recording `why`. Returns whether it was.
pub fn resume_exception(thread: &Arc<Thread>, why: u32) -> bool {
    if thread
        .state
        .compare_exchange(EXCEPTION, RUNNABLE, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    thread.woken.store(why, Ordering::Release);
    queue(thread);
    true
}

/// Ends the running thread if it is DEAD. Called before every return
/// to user mode.
pub fn exit_if_dead() {
    // Endpoints that the call or interrupt left reachable only through
    // queued messages are destroyed before it returns.
    kernel_core::object::channel::collect_if_needed();
    if current().is_some_and(|t| t.state() == DEAD) {
        exit();
    }
}

/// Runs on a new thread's stack after its first switch.
pub extern "sysv64" fn thread_start(entry: usize, arg: usize) -> ! {
    finish_switch();
    // SAFETY: `spawn` stored a `fn(usize)` as the entry.
    let entry: fn(usize) = unsafe { core::mem::transmute(entry) };
    entry(arg);
    exit()
}

/// The running thread.
/// The topic and size of the thread information record.
pub const INFO_THREAD: u32 = 3;
pub const INFO_THREAD_SIZE: usize = 0x18;

/// The nanoseconds the running thread has run in its current slice, when
/// `matches` accepts it, and 0 otherwise.
pub fn running_slice(matches: impl Fn(&Thread) -> bool) -> i64 {
    let cpu = this_cpu().lock();
    match &cpu.current {
        Some(t) if matches(t) => crate::time::now() - cpu.slice_start,
        _ => 0,
    }
}

pub fn current() -> Option<Arc<Thread>> {
    this_cpu().lock().current.clone()
}

/// A user segment base of a thread.
pub enum UserBase {
    Fs,
    Gs,
}

/// Sets a user segment base of the calling user thread. Its current
/// values are in the MSRs until `switch_out` saves them.
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
fn user_entry(_: usize) {
    let thread = this_cpu().lock().current.clone();
    let Some(thread) = thread else {
        unreachable!("a user thread started without being current");
    };
    let [rip, rsp, arg1, arg2] = thread.user.lock().start;
    let report = thread.process().is_some_and(|p| p.debug_events());
    if !report && !thread.suspend_pending() {
        drop(thread);
        exit_if_dead();
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
    exit_if_dead();
    // SAFETY: as above; the handler could change only what
    // `thread_write_state` allows.
    unsafe { context::enter_user_frame(&frame) }
}

/// Switches threads, before an interrupt returns to user mode, if the
/// running thread's slice has ended. Log readers whose notification was
/// left pending are notified first.
pub fn preempt_user() {
    crate::log::notify_pending();
    if reschedule().swap(false, Ordering::Relaxed) {
        schedule(true);
    }
}

/// Starts the kernel's maintenance deadlines, once the clock and the
/// random number generator are set up. The timer serves them whether or
/// not any thread runs.
pub fn start_maintenance() {
    let now = crate::time::now();
    let mut cpu = this_cpu().lock();
    cpu.clock_due = crate::time::maintenance_interval().map_or(INFINITE, |i| now.saturating_add(i));
    cpu.reseed_due = crate::random::reseed_due();
    arm(&cpu);
}

/// Serves the maintenance deadlines that `now` has reached and sets the
/// next ones, which the caller arms.
fn maintain(now: i64) {
    let (clock, reseed) = {
        let cpu = this_cpu().lock();
        (now >= cpu.clock_due, now >= cpu.reseed_due)
    };
    if clock {
        crate::time::maintain();
        #[cfg(feature = "fuzz")]
        crate::fuzz::served(crate::fuzz::CLOCK, now);
        let next = crate::time::maintenance_interval().map_or(INFINITE, |i| now.saturating_add(i));
        this_cpu().lock().clock_due = next;
    }
    if reseed {
        let next = crate::random::reseed(now);
        #[cfg(feature = "fuzz")]
        crate::fuzz::served(crate::fuzz::RESEED, now);
        this_cpu().lock().reseed_due = next;
    }
}

/// Handles the timer interrupt: serves the maintenance deadlines, wakes
/// sleepers whose deadline has passed, and marks a reschedule when the
/// running thread's slice has ended.
pub fn timer_interrupt() {
    let now = crate::time::now();
    maintain(now);
    let mut cpu = this_cpu().lock();
    while let Some(sleeper) = cpu.sleepers.pop_expired(now) {
        let thread = sleeper.take();
        thread.state.store(RUNNABLE, Ordering::Release);
        let entity = *thread.entity.lock();
        // Every thread has room in the queue from when it was created.
        cpu.enqueue(Queued::new(thread), entity);
    }
    if cpu.current.is_some() && now >= cpu.slice_end {
        reschedule().store(true, Ordering::Relaxed);
    }
    arm(&cpu);
    // Expired timers are signalled with the lock released, as setting a
    // signal can wake threads, a few at a time so that nothing allocates.
    loop {
        let mut expired: [Option<crate::ktimer::Armed>; 8] = Default::default();
        for slot in &mut expired {
            *slot = cpu.timers.pop_expired(now);
        }
        drop(cpu);
        let more = expired[7].is_some();
        for timer in expired.into_iter().flatten() {
            timer.fire();
        }
        if !more {
            return;
        }
        cpu = this_cpu().lock();
    }
}

/// Gives `thread` `priority`, which changes its weight from its next
/// slice on; a queued thread is queued again with the new weight.
pub fn set_priority(thread: &Arc<Thread>, priority: u32) {
    let mut cpu = this_cpu().lock();
    thread.priority.store(priority, Ordering::Relaxed);
    let weight = kernel_core::sched::weight(priority);
    let queued = cpu.fair.reweigh(Queued(Arc::as_ptr(thread)), weight);
    let running = cpu.current.as_ref().is_some_and(|c| Arc::ptr_eq(c, thread));
    if !queued && running {
        cpu.fair.reweigh_running(weight);
    }
    thread.entity.lock().weight = weight;
}

/// Arms `timer` for `deadline`, replacing an earlier deadline of it.
pub fn arm_timer(timer: crate::ktimer::Armed, deadline: i64) -> Result<(), Error> {
    let mut cpu = this_cpu().lock();
    cpu.timers.remove(&timer);
    cpu.timers
        .insert(deadline, timer)
        .map_err(|_| Error::NoMemory)?;
    arm(&cpu);
    Ok(())
}

/// Removes the deadline of `timer`, if it has one.
pub fn disarm_timer(timer: &crate::ktimer::Armed) {
    let mut cpu = this_cpu().lock();
    cpu.timers.remove(timer);
    arm(&cpu);
}

/// Runs queued threads, and halts until an interrupt when there are none.
/// The boot processor calls this at the end of boot.
pub fn run() -> ! {
    loop {
        // A pending notification, as of a record that an interrupt
        // wrote, may wake threads, so it comes before the queue is
        // looked at.
        crate::log::notify_pending();
        let now = crate::time::now();
        let mut cpu = this_cpu().lock();
        match cpu.fair.pick() {
            Some((next, entity)) => {
                let next = next.take();
                *next.entity.lock() = entity;
                next.switch_in();
                // SAFETY: `next` is not running.
                let rsp = unsafe { *next.rsp.get() };
                start_slice(&mut cpu, now, next);
                let save = &raw mut cpu.idle_rsp;
                drop(cpu);
                // SAFETY: the idle loop's stack stays mapped, and the
                // per-CPU state that holds its pointer is static.
                unsafe { context::switch(save, rsp) };
                finish_switch();
            }
            // A collection left needed runs before the processor idles;
            // what it destroys may wake threads, so the queue is looked
            // at again after it.
            None if kernel_core::object::channel::collection_pending() => {
                drop(cpu);
                kernel_core::object::channel::collect_if_needed();
            }
            None => {
                arm(&cpu);
                drop(cpu);
                halt_until_interrupt();
            }
        }
    }
}

/// Halts the idle processor until an interrupt arrives. It is a function
/// of its own so that the fuzz build's coverage test can find its code.
#[inline(never)]
fn halt_until_interrupt() {
    // SAFETY: interrupts are enabled until the next one arrives; HLT then
    // returns and they are disabled again.
    unsafe { core::arch::asm!("sti", "hlt", "cli", options(nomem, nostack)) };
}
