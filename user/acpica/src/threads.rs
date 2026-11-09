//! The threads of ACPICA's operating system layer: two that run the work
//! that `AcpiOsExecute` queues, one for notifications and one for the
//! rest, so that a notification handler that waits cannot hold up the
//! general purpose events, and those that wait for interrupts. Each has a
//! stack of its own, by which `AcpiOsGetThreadId` tells the threads apart
//! without a system call.

use crate::sync::{FOREVER, Locked, Semaphore, wait, wake};
use alloc::collections::VecDeque;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use mikros_rt::sys;

const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
/// The stack of each thread: the interpreter's frames are small, but AML
/// methods that call methods nest them.
const STACK_SIZE: u64 = 256 << 10;
/// The most threads that the layer starts.
const MAX_THREADS: usize = 8;
/// The ID of every thread that the layer did not start: the server's, on
/// which ACPICA is started and called.
const SERVER_THREAD: u64 = 1;

/// The lowest address and the end of each started thread's stack, 0
/// and 0 for a slot without one; the ID of a thread is its index plus 2.
static STACKS: [(AtomicU64, AtomicU64); MAX_THREADS] =
    [const { (AtomicU64::new(0), AtomicU64::new(0)) }; MAX_THREADS];
/// The next slot of `STACKS` to take.
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// The ID of the calling thread.
pub fn current() -> u64 {
    let sp: u64;
    // SAFETY: reading the stack pointer touches no memory.
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack, preserves_flags))
    };
    STACKS
        .iter()
        .position(|(low, end)| {
            (low.load(Ordering::Acquire)..end.load(Ordering::Acquire)).contains(&sp)
        })
        .map_or(SERVER_THREAD, |i| i as u64 + 2)
}

/// Starts a thread of `process` named `name` that runs `entry(arg)` and
/// then exits: false when it did not start.
pub fn spawn(process: sys::Handle, name: &[u8], entry: fn(usize), arg: usize) -> bool {
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    if index >= MAX_THREADS {
        return false;
    }
    let mut stack = 0;
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::mem_create(STACK_SIZE, 0, &mut stack) } != mikros_rt::OK {
        return false;
    }
    let mut low = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let mapped = unsafe {
        sys::vm_map(
            process,
            PERM_READ | PERM_WRITE,
            0,
            stack,
            0,
            STACK_SIZE,
            &mut low,
        )
    };
    // The mapping keeps the stack's memory.
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(stack) };
    if mapped != mikros_rt::OK {
        return false;
    }
    let mut thread = 0;
    // SAFETY: the name and the out pointer are valid for the call.
    let created =
        unsafe { sys::thread_create(process, name.as_ptr(), name.len() as u64, &mut thread) };
    if created != mikros_rt::OK {
        // SAFETY: nothing uses the range.
        unsafe { sys::vm_unmap(process, low, STACK_SIZE) };
        return false;
    }
    // The thread sees its slot once it starts, after these stores.
    STACKS[index].0.store(low, Ordering::Release);
    STACKS[index].1.store(low + STACK_SIZE, Ordering::Release);
    // SAFETY: the stack is mapped and unused, 8 bytes below a 16-byte
    // boundary as a function's entry expects.
    let started = unsafe {
        sys::thread_start(
            thread,
            run as *const () as u64,
            low + STACK_SIZE - 8,
            entry as usize as u64,
            arg as u64,
        )
    };
    // SAFETY: closing takes no pointers; the thread runs without it.
    unsafe { sys::handle_close(thread) };
    started == mikros_rt::OK
}

extern "sysv64" fn run(entry: u64, arg: u64) -> ! {
    // SAFETY: `spawn` passes a `fn(usize)` as `entry`.
    let entry: fn(usize) = unsafe { core::mem::transmute(entry as usize) };
    entry(arg as usize);
    // SAFETY: the call takes no pointers; nothing of this thread is used
    // after it.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// A function that ACPICA queues, with its argument.
pub type Callback = unsafe extern "C" fn(*mut c_void);

/// Work for one thread, which it runs in the order it was queued.
struct Queue {
    work: Locked<VecDeque<(Callback, usize)>>,
    ready: Semaphore,
}

impl Queue {
    const fn new() -> Queue {
        Queue {
            work: Locked::new(VecDeque::new()),
            ready: Semaphore::new(u32::MAX, 0),
        }
    }
}

static NOTIFICATIONS: Queue = Queue::new();
static EVENTS: Queue = Queue::new();
/// The work queued that has not finished.
static PENDING: AtomicU32 = AtomicU32::new(0);

/// Starts the threads of the two queues: false when one did not start.
pub fn start(process: sys::Handle) -> bool {
    spawn(
        process,
        b"acpi-notify",
        serve,
        &NOTIFICATIONS as *const Queue as usize,
    ) && spawn(
        process,
        b"acpi-events",
        serve,
        &EVENTS as *const Queue as usize,
    )
}

fn serve(queue: usize) {
    // SAFETY: `start` passes one of the static queues.
    let queue = unsafe { &*(queue as *const Queue) };
    loop {
        queue.ready.take(1, FOREVER);
        let Some((callback, context)) = queue.work.lock().pop_front() else {
            continue;
        };
        // SAFETY: ACPICA queued the function with its argument.
        unsafe { callback(context as *mut c_void) };
        if PENDING.fetch_sub(1, Ordering::AcqRel) == 1 {
            wake(&PENDING, u32::MAX);
        }
    }
}

/// Queues `callback(context)` for the notifications' thread when
/// `notification` is set, and for the other thread otherwise.
pub fn execute(notification: bool, callback: Callback, context: *mut c_void) {
    let queue = if notification {
        &NOTIFICATIONS
    } else {
        &EVENTS
    };
    PENDING.fetch_add(1, Ordering::AcqRel);
    queue.work.lock().push_back((callback, context as usize));
    queue.ready.give(1);
}

/// Waits until all queued work has finished.
pub fn wait_complete() {
    loop {
        let pending = PENDING.load(Ordering::Acquire);
        if pending == 0 {
            return;
        }
        wait(&PENDING, pending, FOREVER);
    }
}
