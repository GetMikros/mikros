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
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use kernel_core::deadline::{Deadlines, INFINITE};
use kernel_core::object::{Header, Name, Object, ObjectType};
use kernel_core::sched::{DEFAULT_PRIORITY, Entity, Fair};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

const IA32_FS_BASE: u32 = 0xC000_0100;

/// The size of a kernel thread's stack.
pub const KERNEL_STACK_SIZE: u64 = 32 << 10;

pub const NEW: u32 = 0;
pub const RUNNABLE: u32 = 1;
pub const BLOCKED: u32 = 2;
pub const DEAD: u32 = 4;

pub struct Thread {
    header: Header,
    #[allow(dead_code, reason = "names are only for diagnostics, which nothing prints yet")]
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
    entity: SpinLock<Entity>,
    /// Nanoseconds the thread has run.
    cpu_time: AtomicI64,
    user: SpinLock<UserState>,
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
        Ok(Arc::new(Thread {
            header: Header::new(ObjectType::Thread),
            name,
            process,
            charged,
            state: AtomicU32::new(state),
            rsp: UnsafeCell::new(0),
            extended,
            stack_top: AtomicU64::new(stack + KERNEL_STACK_SIZE),
            entity: SpinLock::new(Entity::new(DEFAULT_PRIORITY)),
            cpu_time: AtomicI64::new(0),
            user: SpinLock::new(UserState::default()),
        }))
    }

    /// A user thread of `process` in state `NEW`. Its memory is charged
    /// to the process's budget when `charge` is set.
    pub fn new_user(process: Arc<Process>, name: Name, charge: bool) -> Result<Arc<Thread>, Error> {
        let charged = if charge {
            let size = size_of::<Thread>() as u64
                + KERNEL_STACK_SIZE
                + context::extended_state_size() as u64;
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

    #[allow(dead_code, reason = "names are only for diagnostics, which nothing prints yet")]
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
        cpu.fair.enqueue(Queued::new(thread.clone()), entity);
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
        }
    }

    /// Loads the user state of the thread about to run: its extended
    /// state, and for a user thread its address space, its kernel stack
    /// for entries from user mode, and its FS and GS bases.
    fn switch_in(&self) {
        self.extended.restore();
        let Some(process) = &self.process else {
            return;
        };
        process.aspace().activate();
        let top = self.stack_top.load(Ordering::Relaxed);
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

    #[allow(dead_code, reason = "nothing starts kernel threads yet")]
    pub fn cpu_time(&self) -> i64 {
        self.cpu_time.load(Ordering::Relaxed)
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
        }
    }
}

fn this_cpu() -> &'static SpinLock<CpuSched> {
    &percpu::this().sched
}

/// Set by the timer interrupt when the running thread's slice has ended.
static RESCHEDULE: AtomicBool = AtomicBool::new(false);

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
    cpu.fair.enqueue(Queued::new(thread.clone()), entity);
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
    let now = crate::time::now();
    let mut cpu = this_cpu().lock();
    let Some(current) = cpu.current.take() else {
        return;
    };
    let ran = now - cpu.slice_start;
    current.cpu_time.fetch_add(ran, Ordering::Relaxed);
    cpu.fair.charge(ran);
    // A thread made DEAD by another goes on running until it reaches
    // `exit_if_dead`, so it is queued again like a runnable one.
    if requeue && current.state() != BLOCKED {
        let entity = *current.entity.lock();
        // Room for the running thread was reserved when it was queued.
        cpu.fair.enqueue(Queued::new(current.clone()), entity);
    }
    let picked = cpu.fair.pick();
    if let Some((next, entity)) = picked
        && next.0 == Arc::as_ptr(&current)
    {
        // The running thread goes on.
        *current.entity.lock() = entity;
        drop(next.take());
        start_slice(&mut cpu, now, current, entity);
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
            start_slice(&mut cpu, now, next, entity);
            rsp
        }
        None => cpu.idle_rsp,
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
/// timer for it.
fn start_slice(cpu: &mut CpuSched, now: i64, thread: Arc<Thread>, entity: Entity) {
    cpu.current = Some(thread);
    cpu.slice_start = now;
    cpu.slice_end = now.saturating_add(cpu.fair.slice(entity.weight));
    arm(cpu);
}

/// Arms the timer for the earlier of the running thread's slice end and
/// the nearest deadline, or stops it when there is neither.
fn arm(cpu: &CpuSched) {
    let slice_end = if cpu.current.is_some() {
        cpu.slice_end
    } else {
        INFINITE
    };
    let at = cpu
        .sleepers
        .next_finite()
        .map_or(slice_end, |d| d.min(slice_end));
    if at == INFINITE {
        crate::timer::disarm();
    } else {
        crate::timer::arm(at);
    }
}

#[allow(dead_code, reason = "nothing starts kernel threads yet")]
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

/// Gives up the processor to the next queued thread, if any.
#[allow(dead_code, reason = "nothing starts kernel threads yet")]
pub fn yield_now() {
    schedule(true);
}

/// Lets pending interrupts in, and switches threads if the running
/// thread's slice has ended.
#[allow(dead_code, reason = "nothing starts kernel threads yet")]
pub fn preempt_point() {
    // SAFETY: enabling interrupts for one instruction lets the processor
    // take pending ones; no lock is held here.
    unsafe { core::arch::asm!("sti", "nop", "cli", options(nomem, nostack)) };
    if RESCHEDULE.swap(false, Ordering::Relaxed) {
        schedule(true);
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
        // The reference the sleeper held moves to the run queue.
        if cpu.sleepers.remove(&queued) {
            let entity = *thread.entity.lock();
            cpu.fair.enqueue(queued, entity);
        }
    }
    was
}

/// Ends the running thread if it is DEAD. Called before every return
/// to user mode.
pub fn exit_if_dead() {
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
    drop(thread);
    exit_if_dead();
    // SAFETY: `switch_in` loaded the thread's address space, extended
    // state and user bases before switching to it.
    unsafe { context::enter_user(rip, rsp, arg1, arg2) }
}

/// Switches threads, before an interrupt returns to user mode, if the
/// running thread's slice has ended.
pub fn preempt_user() {
    if RESCHEDULE.swap(false, Ordering::Relaxed) {
        schedule(true);
    }
}

/// Handles the timer interrupt: wakes sleepers whose deadline has
/// passed, and marks a reschedule when the running thread's slice has
/// ended.
pub fn timer_interrupt() {
    let now = crate::time::now();
    let mut cpu = this_cpu().lock();
    while let Some(sleeper) = cpu.sleepers.pop_expired(now) {
        let thread = sleeper.take();
        thread.state.store(RUNNABLE, Ordering::Release);
        let entity = *thread.entity.lock();
        // Every thread has room in the queue from when it was created.
        cpu.fair.enqueue(Queued::new(thread), entity);
    }
    if cpu.current.is_some() && now >= cpu.slice_end {
        RESCHEDULE.store(true, Ordering::Relaxed);
    }
    arm(&cpu);
}

/// Runs queued threads, and halts until an interrupt when there are none.
/// The boot processor calls this at the end of boot.
pub fn run() -> ! {
    loop {
        let now = crate::time::now();
        let mut cpu = this_cpu().lock();
        match cpu.fair.pick() {
            Some((next, entity)) => {
                let next = next.take();
                *next.entity.lock() = entity;
                next.switch_in();
                // SAFETY: `next` is not running.
                let rsp = unsafe { *next.rsp.get() };
                start_slice(&mut cpu, now, next, entity);
                let save = &raw mut cpu.idle_rsp;
                drop(cpu);
                // SAFETY: the idle loop's stack stays mapped, and the
                // per-CPU state that holds its pointer is static.
                unsafe { context::switch(save, rsp) };
                finish_switch();
            }
            None => {
                arm(&cpu);
                drop(cpu);
                // SAFETY: interrupts are enabled until the next one
                // arrives; HLT then returns and they are disabled again.
                unsafe { core::arch::asm!("sti", "hlt", "cli", options(nomem, nostack)) };
            }
        }
    }
}
