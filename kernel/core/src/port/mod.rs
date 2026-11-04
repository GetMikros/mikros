//! Ports: queues of packets, filled by asynchronous waits on objects and
//! by user space. `queue` keeps the queues and the
//! registrations of the waits; this adds the charges, the waiting threads
//! and the watches of the objects waited on.

mod queue;
pub use queue::*;

use crate::object::budget::{Budget, Held};
use crate::object::handles::Handle;
use crate::object::{Header, Object, ObjectType};
use crate::process::Process;
use crate::sched::{self, Thread};
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
pub use queue::{PACKET_SIZE, packet};

/// The kernel memory a registration is charged: itself and its packet.
const REGISTRATION_COST: u64 =
    (size_of::<Registration<Kernel>>() + size_of::<Packet<Held>>()) as u64;
/// The kernel memory a queued user packet is charged.
const USER_COST: u64 = size_of::<Packet<Held>>() as u64;
/// The kernel memory charged to a thread's process for the length of
/// its `port_wait`: its place in the port's waiters and that place's
/// share of their storage.
const WAITER_COST: u64 = 2 * size_of::<Arc<Thread>>() as u64;

struct State {
    queue: Queue<Held>,
    /// The threads in `port_wait`.
    waiters: Vec<Arc<Thread>>,
}

impl State {
    /// Wakes the waiting threads, as a packet was queued.
    fn wake(&self) {
        for thread in &self.waiters {
            sched::wake_sleeper(thread);
        }
    }
}

pub struct Port {
    header: Header,
    budget: Arc<Budget>,
    state: SpinLock<State>,
}

impl Port {
    const COST: u64 = size_of::<Port>() as u64;

    /// An empty port charged to `budget`.
    pub fn new(budget: &Arc<Budget>) -> Result<Arc<Port>, Error> {
        budget.charge_kernel(Self::COST)?;
        crate::fallible::try_arc(Port {
            header: Header::new(ObjectType::Port),
            budget: budget.clone(),
            state: SpinLock::new(State {
                queue: Queue::new(),
                waiters: Vec::new(),
            }),
        })
    }

    /// Queues a user packet, as `port_queue` does, charged to `budget`
    /// until it is read.
    pub fn queue_user(&self, bytes: &[u8; PACKET_SIZE], budget: &Arc<Budget>) -> Result<(), Error> {
        let mut state = self.state.lock();
        state
            .queue
            .queue_user(bytes, || budget.hold_kernel(USER_COST))?;
        state.wake();
        Ok(())
    }

    /// Queues a packet that the kernel makes, such as a page request,
    /// charged to `budget` until it is read.
    pub fn queue_kernel(
        &self,
        bytes: [u8; PACKET_SIZE],
        budget: &Arc<Budget>,
    ) -> Result<(), Error> {
        let mut state = self.state.lock();
        state
            .queue
            .queue_kernel(bytes, || budget.hold_kernel(USER_COST))?;
        state.wake();
        Ok(())
    }

    /// Waits until `deadline` for a packet to be queued, leaving it
    /// queued, so that the caller can prepare its output before `take`.
    pub fn wait_queued(&self, deadline: i64) -> Result<(), Error> {
        let thread = sched::current().ok_or(Error::BadState)?;
        let process = thread.process().cloned().ok_or(Error::BadState)?;
        let _charge = process.budget().hold_kernel(WAITER_COST)?;
        {
            let mut state = self.state.lock();
            state.waiters.try_reserve(1).map_err(|_| Error::NoMemory)?;
            state.waiters.push(thread.clone());
        }
        let result = loop {
            if !self.state.lock().queue.is_empty() {
                break Ok(());
            }
            if crate::signal::interrupted(&thread, &process) || thread.leave_wait_to_suspend() {
                break Err(Error::Interrupted);
            }
            if thread.state() == sched::DEAD || deadline <= crate::arch::monotonic_now() {
                break Err(Error::TimedOut);
            }
            if let Err(e) = sched::wait_for_signals(&thread, deadline) {
                break Err(e);
            }
        };
        self.state
            .lock()
            .waiters
            .retain(|t| !Arc::ptr_eq(t, &thread));
        result
    }

    /// Removes the oldest packet, if any.
    pub fn take(&self) -> Option<[u8; PACKET_SIZE]> {
        // The packet's charge goes after the lock.
        let packet = self.state.lock().queue.take()?;
        if let Some(Origin { id, .. }) = packet.origin {
            // Its registration may queue again.
            REGISTRATIONS.lock().packet_read(id);
        }
        Some(packet.bytes)
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        self.budget.release_kernel(Self::COST);
    }
}

impl Object for Port {
    fn header(&self) -> &Header {
        &self.header
    }
}

/// What the kernel keeps with a registration.
struct Kernel {
    port: Weak<Port>,
    /// The object waited on, to end its watch when the registration is
    /// removed other than by its signals.
    object: Weak<dyn Object>,
    budget: Arc<Budget>,
}

impl Target for Kernel {
    fn exists(&self) -> bool {
        self.port.strong_count() > 0
    }

    fn deliver(&self, bytes: [u8; PACKET_SIZE], origin: Origin) {
        let Some(port) = self.port.upgrade() else {
            return;
        };
        let mut state = port.state.lock();
        state.queue.queue_registered(bytes, origin);
        state.wake();
        drop(state);
        // The port is not the last reference: the caller's handle or a
        // registration's queue holds it while the hook runs.
        drop(port);
    }
}

impl Kernel {
    /// Ends the registration's hold on the port's queue and its charge.
    fn release(&self) {
        if let Some(port) = self.port.upgrade() {
            port.state.lock().queue.remove_registration();
        }
        self.budget.release_kernel(REGISTRATION_COST);
    }

    /// Ends the watch of the object of `wait`, if it still exists and the
    /// wait watches signals.
    fn end_watch(&self, wait: &Wait) {
        if wait.threshold.is_some() {
            return;
        }
        // Upgraded only with no lock held, as dropping the reference may
        // destroy the object.
        if let Some(object) = self.object.upgrade() {
            object.header().unwatch();
        }
    }
}

/// Every registration. The lock comes after object locks and before port
/// and scheduler locks.
static REGISTRATIONS: SpinLock<Registrations<Kernel>> = SpinLock::new(Registrations::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Called by the signal hook for the object of `header`: fires the
/// registrations whose signals are now set. Allocates and drops nothing.
pub fn signals_changed(header: &Header, signals: u32) {
    let ended = REGISTRATIONS.lock().signals_changed(header.koid(), signals);
    for _ in 0..ended {
        header.unwatch();
    }
}

/// Removes the registrations that `matches` accepts and returns them,
/// releasing at once those there is no room to return.
fn remove(matches: impl Fn(&Registration<Kernel>) -> bool) -> Vec<Registration<Kernel>> {
    let mut removed = Vec::new();
    let mut registrations = REGISTRATIONS.lock();
    let count = registrations.iter().filter(|r| matches(r)).count();
    let _ = removed.try_reserve_exact(count);
    registrations.take(matches, &mut removed, |r| r.extra.release());
    removed
}

/// Removes the registrations that ended, releasing their charges. They
/// keep the charges until the list of them goes, so those cover it.
fn purge() {
    if !REGISTRATIONS.lock().any_ended() {
        return;
    }
    for r in remove(|r| r.is_done()) {
        r.extra.release();
    }
}

/// Ends the watches of the `removed` registrations that had not ended,
/// and releases them all.
fn finish(removed: Vec<Registration<Kernel>>) {
    for r in removed {
        if !r.is_done() {
            r.extra.end_watch(&r.wait);
        }
        r.extra.release();
    }
}

/// A new registration on `object` through `handle` of `process`, with
/// its room in the port's queue reserved and its charge made.
fn reserve(
    object: &Arc<dyn Object>,
    process: &Arc<Process>,
    handle: Handle,
    port: &Arc<Port>,
    key: u64,
    mask: u32,
    threshold: Option<u64>,
) -> Result<Registration<Kernel>, Error> {
    purge();
    let budget = process.budget().clone();
    port.state
        .lock()
        .queue
        .add_registration(|| budget.charge_kernel(REGISTRATION_COST))?;
    let wait = Wait {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        koid: object.header().koid(),
        mask,
        edge: false,
        key,
        process: Arc::as_ptr(process) as usize,
        handle,
        threshold,
    };
    let kernel = Kernel {
        port: Arc::downgrade(port),
        object: Arc::downgrade(object),
        budget,
    };
    Ok(Registration::new(wait, kernel))
}

/// Registers a wait on the timeline `object`, whose value is `current`,
/// for `value`, through `handle` of `process`, as `timeline_wait_async`
/// does: its packet is queued at once when `current` reaches `value`.
pub fn wait_timeline(
    object: &Arc<dyn Object>,
    current: u64,
    process: &Arc<Process>,
    handle: Handle,
    port: &Arc<Port>,
    key: u64,
    value: u64,
) -> Result<(), Error> {
    let mut registration = reserve(object, process, handle, port, key, 0, Some(value))?;
    if current >= value {
        registration.queue(TIMELINE, &current.to_le_bytes());
        registration.extra.release();
        return Ok(());
    }
    let mut registrations = REGISTRATIONS.lock();
    if registrations.try_reserve().is_err() {
        drop(registrations);
        registration.extra.release();
        return Err(Error::NoMemory);
    }
    registrations.push(registration);
    Ok(())
}

/// Queues the packets of the timeline waits on the timeline with object
/// ID `koid` that its new value `value` reaches.
pub fn timeline_raised(koid: u64, value: u64) {
    REGISTRATIONS.lock().timeline_raised(koid, value);
}

/// Registers an asynchronous wait on `object` through `handle` of
/// `process`, as `object_wait_async` does.
pub fn wait_async(
    object: &Arc<dyn Object>,
    process: &Arc<Process>,
    handle: Handle,
    port: &Arc<Port>,
    key: u64,
    signals: u32,
    options: u32,
) -> Result<(), Error> {
    if options & !EDGE != 0 {
        return Err(Error::InvalidArgs);
    }
    let mut registration = reserve(object, process, handle, port, key, signals, None)?;
    registration.wait.edge = options & EDGE != 0;
    let header = object.header();
    let mut registrations = REGISTRATIONS.lock();
    if registrations.try_reserve().is_err() {
        drop(registrations);
        registration.extra.release();
        return Err(Error::NoMemory);
    }
    header.watch();
    if let Some(ended) = registrations.add_signal_wait(registration, header.signals()) {
        header.unwatch();
        drop(registrations);
        ended.extra.release();
    }
    Ok(())
}

/// Removes the registrations made through `handle` of the process at
/// `process` without packets, as the handle leaves the table. With
/// `handle` `None`, removes every registration of the process. The
/// charges of the removed registrations cover the list of them.
pub fn handle_closed(process: usize, handle: Option<Handle>) {
    finish(remove(|r| {
        r.wait.process == process && handle.is_none_or(|h| h == r.wait.handle)
    }));
    purge();
}

/// Removes the registrations made on `port` through `handle` with `key`,
/// with their unread packets, as `port_cancel` does. The charges of the
/// removed registrations cover the list of them.
pub fn cancel(port: &Arc<Port>, process: usize, handle: Handle, key: u64) {
    let removed = remove(|r| {
        let w = &r.wait;
        let ours = r.extra.port.as_ptr() == Arc::as_ptr(port);
        ours && w.process == process && w.handle == handle && w.key == key
    });
    // Packets of one-shot registrations that already ended go too.
    port.state.lock().queue.cancel(process, handle, key);
    finish(removed);
    purge();
}
