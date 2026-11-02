//! Kernel threads and the scheduler of this processor.
//!
//! Kernel code runs with interrupts disabled. They are enabled only while
//! the processor idles and at preemption points, so the scheduler's state
//! is never touched from an interrupt that arrives in the middle of a
//! change to it.

use crate::context::{self, ExtendedState};
use crate::percpu;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use kernel_core::sched::{DEFAULT_PRIORITY, Entity, Fair};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// The size of a kernel thread's stack.
pub const KERNEL_STACK_SIZE: u64 = 32 << 10;

pub const RUNNABLE: u32 = 1;
pub const DEAD: u32 = 4;

pub struct Thread {
    state: AtomicU32,
    /// The stack pointer saved while the thread does not run.
    rsp: UnsafeCell<u64>,
    extended: ExtendedState,
    stack_top: u64,
    entity: SpinLock<Entity>,
    /// Nanoseconds the thread has run.
    cpu_time: AtomicI64,
}

// SAFETY: `rsp` is written only by `context_switch` while switching away
// from the thread and read only while switching to it, on the processor
// that runs it, with interrupts disabled.
unsafe impl Sync for Thread {}

impl Thread {
    pub fn state(&self) -> u32 {
        self.state.load(Ordering::Acquire)
    }

    #[allow(dead_code, reason = "nothing starts kernel threads yet")]
    pub fn cpu_time(&self) -> i64 {
        self.cpu_time.load(Ordering::Relaxed)
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
    let extended = ExtendedState::new().ok_or(Error::NoMemory)?;
    let stack = crate::space::alloc_pages(KERNEL_STACK_SIZE).ok_or(Error::NoMemory)?;
    let top = stack + KERNEL_STACK_SIZE;
    // SAFETY: the stack was just mapped and nothing uses it.
    let rsp = unsafe { context::prepare(top, entry as usize as u64, arg as u64) };
    let thread = Arc::new(Thread {
        state: AtomicU32::new(RUNNABLE),
        rsp: UnsafeCell::new(rsp),
        extended,
        stack_top: top,
        entity: SpinLock::new(Entity::new(DEFAULT_PRIORITY)),
        cpu_time: AtomicI64::new(0),
    });
    let mut cpu = this_cpu().lock();
    if cpu.fair.try_reserve(1).is_err() {
        drop(cpu);
        free_stack(&thread);
        return Err(Error::NoMemory);
    }
    let entity = *thread.entity.lock();
    cpu.fair.enqueue(Queued::new(thread.clone()), entity);
    Ok(thread)
}

fn free_stack(thread: &Thread) {
    crate::space::free_pages(thread.stack_top - KERNEL_STACK_SIZE, KERNEL_STACK_SIZE);
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
    if requeue && current.state() == RUNNABLE {
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
        start_slice(&mut cpu, now, entity);
        cpu.current = Some(current);
        return;
    }
    let save = current.rsp.get();
    current.extended.save();
    let load = match picked {
        Some((next, entity)) => {
            let next = next.take();
            *next.entity.lock() = entity;
            next.extended.restore();
            // SAFETY: `next` is not running, so its saved pointer is
            // stable.
            let rsp = unsafe { *next.rsp.get() };
            start_slice(&mut cpu, now, *next.entity.lock());
            cpu.current = Some(next);
            rsp
        }
        None => cpu.idle_rsp,
    };
    if current.state() == DEAD {
        cpu.dead = Some(current.clone());
    }
    drop(cpu);
    drop(current);
    // SAFETY: `load` is a thread's saved stack or the idle loop's, and
    // `save` belongs to a thread that the scheduler still references.
    unsafe { context::switch(save, load) };
    finish_switch();
}

fn start_slice(cpu: &mut CpuSched, now: i64, entity: Entity) {
    cpu.slice_start = now;
    cpu.slice_end = now.saturating_add(cpu.fair.slice(entity.weight));
    crate::timer::arm(cpu.slice_end);
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

/// Runs on a new thread's stack after its first switch.
pub extern "sysv64" fn thread_start(entry: usize, arg: usize) -> ! {
    finish_switch();
    // SAFETY: `spawn` stored a `fn(usize)` as the entry.
    let entry: fn(usize) = unsafe { core::mem::transmute(entry) };
    entry(arg);
    exit()
}

/// Handles the timer interrupt.
pub fn timer_interrupt() {
    let cpu = this_cpu().lock();
    if cpu.current.is_some() && crate::time::now() >= cpu.slice_end {
        RESCHEDULE.store(true, Ordering::Relaxed);
    }
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
                next.extended.restore();
                // SAFETY: `next` is not running.
                let rsp = unsafe { *next.rsp.get() };
                start_slice(&mut cpu, now, entity);
                cpu.current = Some(next);
                let save = &raw mut cpu.idle_rsp;
                drop(cpu);
                // SAFETY: the idle loop's stack stays mapped, and the
                // per-CPU state that holds its pointer is static.
                unsafe { context::switch(save, rsp) };
                finish_switch();
            }
            None => {
                drop(cpu);
                // SAFETY: interrupts are enabled until the next one
                // arrives; HLT then returns and they are disabled again.
                unsafe { core::arch::asm!("sti", "hlt", "cli", options(nomem, nostack)) };
            }
        }
    }
}
