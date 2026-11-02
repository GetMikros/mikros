//! Threads waiting for object signals.

use crate::process::Process;
use crate::sched::{self, Thread};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use kernel_core::object::channel::{Endpoint, Message};
use kernel_core::object::handles::Handle;
use kernel_core::object::{Header, Object, signals};
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
    let waiter = kernel_core::fallible::try_arc(Waiter {
        koid: header.koid(),
        mask,
        thread: thread.clone(),
        process: Arc::as_ptr(process) as usize,
        handle,
        canceled: AtomicBool::new(false),
    })?;
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

/// A thread in `channel_call`, woken when its reply arrives.
struct Caller {
    /// The object ID of the calling endpoint and the transaction ID.
    koid: u64,
    id: u32,
    thread: Arc<Thread>,
}

/// The threads waiting for replies; the lock comes after a handle
/// table's and before the scheduler's.
static CALLERS: SpinLock<Vec<Caller>> = SpinLock::new(Vec::new());

/// Wakes the caller of call `id` on the endpoint with object ID `koid`,
/// whose reply has been delivered.
pub fn reply_delivered(koid: u64, id: u32) {
    for c in CALLERS.lock().iter() {
        if c.koid == koid && c.id == id {
            sched::wake_sleeper(&c.thread);
        }
    }
}

/// Blocks the running thread until the reply of call `id` on `endpoint`
/// arrives, and returns it. Fails with `TimedOut` at `deadline`,
/// `PeerClosed` when the peer goes, and `Interrupted` when a signal ends
/// the call; the call then stays outstanding for the caller to abandon.
pub fn wait_call(
    endpoint: &Arc<Endpoint>,
    process: &Arc<Process>,
    id: u32,
    deadline: i64,
) -> Result<Message, Error> {
    let thread = sched::current().ok_or(Error::BadState)?;
    let header = endpoint.header();
    let waiter = kernel_core::fallible::try_arc(Waiter {
        koid: header.koid(),
        mask: signals::PEER_CLOSED,
        thread: thread.clone(),
        process: 0,
        handle: 0,
        canceled: AtomicBool::new(false),
    })?;
    {
        let mut waiters = WAITERS.lock();
        waiters.try_reserve(1).map_err(|_| Error::NoMemory)?;
        let mut callers = CALLERS.lock();
        callers.try_reserve(1).map_err(|_| Error::NoMemory)?;
        waiters.push(waiter.clone());
        callers.push(Caller {
            koid: header.koid(),
            id,
            thread: thread.clone(),
        });
    }
    header.watch();
    let result = loop {
        if let Some(reply) = endpoint.take_reply(id) {
            break Ok(reply);
        }
        if endpoint.peer_closed() {
            break Err(Error::PeerClosed);
        }
        if crate::signal::call_interrupted(&thread, process) {
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
    CALLERS
        .lock()
        .retain(|c| !(c.koid == header.koid() && c.id == id));
    result
}

/// A thread in `futex_wait`.
struct FutexWaiter {
    /// The memory object's object ID and the offset in it.
    key: (u64, u64),
    thread: Arc<Thread>,
    woken: AtomicBool,
}

/// The threads waiting on futexes, oldest first.
static FUTEXES: SpinLock<Vec<Arc<FutexWaiter>>> = SpinLock::new(Vec::new());

/// The futex key of `addr` in `process`: the object ID of the memory
/// object mapped readable there and the offset in it. Fails with
/// `InvalidArgs` when `addr` is not 4-byte aligned or not mapped
/// readable.
fn futex_key(process: &Process, addr: u64) -> Result<(u64, u64), Error> {
    if !addr.is_multiple_of(4) {
        return Err(Error::InvalidArgs);
    }
    process
        .aspace()
        .with_region(addr, |r| {
            r.filter(|r| r.perms & kernel_core::regions::PERM_READ != 0)
                .map(|r| (r.mem.header().koid(), r.offset + (addr - r.start)))
        })
        .ok_or(Error::InvalidArgs)
}

/// Blocks the running thread on the futex at `addr` if the `u32` there
/// is `expected`, as `futex_wait` does. The comparison and the
/// registration happen in one system call, which nothing else runs
/// during, so no wake can come in between.
pub fn futex_wait(addr: u64, expected: u32, deadline: i64) -> Result<(), Error> {
    let thread = sched::current().ok_or(Error::BadState)?;
    let process = thread.process().cloned().ok_or(Error::BadState)?;
    let key = futex_key(&process, addr)?;
    let value: u32 = crate::syscall::UserIn::<u32>::new(addr)
        .read()
        .map_err(|_| Error::InvalidArgs)?;
    if value != expected {
        return Err(Error::BadState);
    }
    let waiter = kernel_core::fallible::try_arc(FutexWaiter {
        key,
        thread: thread.clone(),
        woken: AtomicBool::new(false),
    })?;
    {
        let mut futexes = FUTEXES.lock();
        futexes.try_reserve(1).map_err(|_| Error::NoMemory)?;
        futexes.push(waiter.clone());
    }
    let result = loop {
        if waiter.woken.load(Ordering::Acquire) {
            break Ok(());
        }
        if crate::signal::interrupted(&thread, &process) {
            break Err(Error::Interrupted);
        }
        if thread.state() == sched::DEAD || deadline <= crate::time::now() {
            break Err(Error::TimedOut);
        }
        if let Err(e) = sched::wait_for_signals(&thread, deadline) {
            break Err(e);
        }
    };
    FUTEXES.lock().retain(|w| !Arc::ptr_eq(w, &waiter));
    // A wake that came together with the end of the wait counts.
    if waiter.woken.load(Ordering::Acquire) {
        return Ok(());
    }
    result
}

/// Wakes up to `count` threads waiting on the futex at `addr`, oldest
/// first, and returns how many, as `futex_wake` does.
pub fn futex_wake(addr: u64, count: u32) -> Result<u32, Error> {
    let process = sched::current()
        .and_then(|t| t.process().cloned())
        .ok_or(Error::BadState)?;
    let key = futex_key(&process, addr)?;
    let mut woken = 0;
    let mut futexes = FUTEXES.lock();
    futexes.retain(|w| {
        if woken == count || w.key != key {
            return true;
        }
        w.woken.store(true, Ordering::Release);
        sched::wake_sleeper(&w.thread);
        woken += 1;
        false
    });
    Ok(woken)
}
