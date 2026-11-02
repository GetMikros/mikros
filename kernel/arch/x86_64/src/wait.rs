//! Threads waiting for object signals.

use crate::process::Process;
use crate::sched::{self, Thread};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use kernel_core::object::handles::Handle;
use kernel_core::object::{Header, Object};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// A thread in `object_wait_one`.
struct Waiter {
    koid: u64,
    mask: u32,
    thread: Arc<Thread>,
    /// The process whose handle the thread waits through, by address,
    /// and the handle.
    process: usize,
    handle: Handle,
    canceled: AtomicBool,
}

/// The waiting threads. The lock comes after object locks and before the
/// scheduler's. Only the waiters themselves add and remove entries, so
/// the hook never drops a thread.
static WAITERS: SpinLock<Vec<Arc<Waiter>>> = SpinLock::new(Vec::new());

/// The signal hook: wakes the threads waiting for a signal now set on
/// the object of `header`.
fn signals_changed(header: &Header, signals: u32) {
    for w in WAITERS.lock().iter() {
        if w.koid == header.koid() && signals & w.mask != 0 {
            sched::wake_sleeper(&w.thread);
        }
    }
    crate::port::signals_changed(header, signals);
}

/// Installs the signal hook.
pub fn init() {
    kernel_core::object::set_signal_hook(signals_changed);
}

/// Ends the waits through `handle` of `process`, which is leaving its
/// handle table, with `Canceled`.
pub fn handle_closed(process: &Process, handle: Handle) {
    let address = process as *const Process as usize;
    for w in WAITERS.lock().iter() {
        if w.process == address && w.handle == handle {
            w.canceled.store(true, Ordering::Release);
            sched::wake_sleeper(&w.thread);
        }
    }
    crate::port::handle_closed(address, Some(handle));
}

/// Removes the asynchronous waits made through the handles of `process`,
/// which has terminated.
pub fn process_ended(process: &Process) {
    crate::port::handle_closed(process as *const Process as usize, None);
}

/// Blocks the running thread, which waits through `handle` of `process`,
/// until a signal in `mask` is set on `object`, and returns the signal
/// state then. Fails with `TimedOut` at `deadline`, `Canceled` when the
/// handle leaves the table and `Interrupted` when a signal handler is to
/// run.
pub fn wait_one(
    object: &Arc<dyn Object>,
    process: &Arc<Process>,
    handle: Handle,
    mask: u32,
    deadline: i64,
) -> Result<u32, Error> {
    let thread = sched::current().ok_or(Error::BadState)?;
    let header = object.header();
    let waiter = Arc::new(Waiter {
        koid: header.koid(),
        mask,
        thread: thread.clone(),
        process: Arc::as_ptr(process) as usize,
        handle,
        canceled: AtomicBool::new(false),
    });
    {
        let mut waiters = WAITERS.lock();
        waiters.try_reserve(1).map_err(|_| Error::NoMemory)?;
        waiters.push(waiter.clone());
    }
    header.watch();
    let result = loop {
        let signals = header.signals();
        if signals & mask != 0 {
            break Ok(signals);
        }
        if waiter.canceled.load(Ordering::Acquire) {
            break Err(Error::Canceled);
        }
        if crate::signal::interrupted(&thread, process) {
            break Err(Error::Interrupted);
        }
        if thread.state() == sched::DEAD || deadline <= crate::time::now() {
            break Err(Error::TimedOut);
        }
        if let Err(e) = sched::wait_for_signals(&thread, deadline) {
            break Err(e);
        }
    };
    header.unwatch();
    WAITERS.lock().retain(|w| !Arc::ptr_eq(w, &waiter));
    result
}
