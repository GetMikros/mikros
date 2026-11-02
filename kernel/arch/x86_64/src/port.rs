//! Ports: queues of packets, filled by asynchronous waits on objects and
//! by user space.

use crate::process::Process;
use crate::sched::{self, Thread};
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use kernel_core::object::budget::Budget;
use kernel_core::object::handles::Handle;
use kernel_core::object::{Header, Object, ObjectType};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// The size of a packet.
pub const PACKET_SIZE: usize = 48;
/// Packet types.
pub const SIGNAL: u32 = 1;
pub const USER: u32 = 2;
pub const TIMELINE: u32 = 4;
/// The `EDGE` option of `object_wait_async`.
pub const EDGE: u32 = 1 << 0;
/// The most `USER` packets queued on a port.
const USER_MAX: usize = 4096;
/// The most registrations of a port.
const REGISTRATIONS_MAX: usize = 65536;
/// The kernel memory a registration is charged: itself and its packet.
const REGISTRATION_COST: u64 = (size_of::<Registration>() + size_of::<Queued>()) as u64;
/// The kernel memory a queued user packet is charged.
const USER_COST: u64 = size_of::<Queued>() as u64;

/// The bytes of a packet.
pub fn packet(key: u64, kind: u32, status: i32, payload: &[u8]) -> [u8; PACKET_SIZE] {
    let mut p = [0u8; PACKET_SIZE];
    p[0x00..0x08].copy_from_slice(&key.to_le_bytes());
    p[0x08..0x0C].copy_from_slice(&kind.to_le_bytes());
    p[0x0C..0x10].copy_from_slice(&status.to_le_bytes());
    p[0x10..0x10 + payload.len()].copy_from_slice(payload);
    p
}

/// A queued packet, with the registration that queued it, or for a user
/// packet the budget it is charged to.
struct Queued {
    bytes: [u8; PACKET_SIZE],
    registration: Option<Origin>,
    charged: Option<Arc<Budget>>,
}

/// The registration that queued a packet, and the process and handle it
/// was made through, which outlive it for a one-shot registration.
#[derive(Clone, Copy)]
struct Origin {
    id: u64,
    process: usize,
    handle: Handle,
}

impl Drop for Queued {
    fn drop(&mut self) {
        if let Some(budget) = &self.charged {
            budget.release_kernel(USER_COST);
        }
    }
}

struct State {
    packets: VecDeque<Queued>,
    /// The live registrations on the port; each may have one packet
    /// queued, for which the queue keeps room.
    registrations: usize,
    users: usize,
    /// The threads in `port_wait`.
    waiters: Vec<Arc<Thread>>,
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
        kernel_core::fallible::try_arc(Port {
            header: Header::new(ObjectType::Port),
            budget: budget.clone(),
            state: SpinLock::new(State {
                packets: VecDeque::new(),
                registrations: 0,
                users: 0,
                waiters: Vec::new(),
            }),
        })
    }

    /// Queues `bytes`, for which the queue has room, and wakes the
    /// waiting threads. Allocates nothing.
    fn push(&self, state: &mut State, packet: Queued) {
        debug_assert!(state.packets.len() < state.packets.capacity());
        state.packets.push_back(packet);
        for thread in &state.waiters {
            sched::wake_sleeper(thread);
        }
    }

    /// Queues a user packet, as `port_queue` does, charged to `budget`
    /// until it is read.
    pub fn queue_user(&self, bytes: &[u8; PACKET_SIZE], budget: &Arc<Budget>) -> Result<(), Error> {
        let mut state = self.state.lock();
        if state.users >= USER_MAX {
            return Err(Error::ShouldWait);
        }
        let room = state.registrations + 1;
        state
            .packets
            .try_reserve(room)
            .map_err(|_| Error::NoMemory)?;
        budget.charge_kernel(USER_COST)?;
        let mut copy = *bytes;
        copy[0x08..0x0C].copy_from_slice(&USER.to_le_bytes());
        copy[0x0C..0x10].fill(0);
        state.users += 1;
        let packet = Queued {
            bytes: copy,
            registration: None,
            charged: Some(budget.clone()),
        };
        self.push(&mut state, packet);
        Ok(())
    }

    /// Queues a packet that the kernel makes, such as a page request.
    pub fn queue_kernel(&self, bytes: [u8; PACKET_SIZE]) -> Result<(), Error> {
        let mut state = self.state.lock();
        let room = state.registrations + 1;
        state
            .packets
            .try_reserve(room)
            .map_err(|_| Error::NoMemory)?;
        let packet = Queued {
            bytes,
            registration: None,
            charged: None,
        };
        // A kernel packet does not count as a user packet when read.
        state.users += 1;
        self.push(&mut state, packet);
        Ok(())
    }

    /// Waits until `deadline` for a packet to be queued, leaving it
    /// queued, so that the caller can prepare its output before `take`.
    pub fn wait_queued(&self, deadline: i64) -> Result<(), Error> {
        let thread = sched::current().ok_or(Error::BadState)?;
        let process = thread.process().cloned().ok_or(Error::BadState)?;
        {
            let mut state = self.state.lock();
            state.waiters.try_reserve(1).map_err(|_| Error::NoMemory)?;
            state.waiters.push(thread.clone());
        }
        let result = loop {
            if !self.state.lock().packets.is_empty() {
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
        self.state
            .lock()
            .waiters
            .retain(|t| !Arc::ptr_eq(t, &thread));
        result
    }

    /// Removes the oldest packet, if any.
    pub fn take(&self) -> Option<[u8; PACKET_SIZE]> {
        let mut state = self.state.lock();
        let p = state.packets.pop_front()?;
        if p.registration.is_none() {
            state.users -= 1;
        }
        drop(state);
        if let Some(Origin { id, .. }) = p.registration {
            // Its registration may queue again.
            for r in REGISTRATIONS.lock().iter() {
                if r.id == id {
                    r.queued.store(false, Ordering::Release);
                }
            }
        }
        Some(p.bytes)
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

/// An asynchronous wait.
struct Registration {
    id: u64,
    koid: u64,
    mask: u32,
    edge: bool,
    key: u64,
    port: Weak<Port>,
    /// The object waited on, to end its watch when the registration is
    /// removed other than by its signals.
    object: Weak<dyn Object>,
    /// The process and handle the wait was made through.
    process: usize,
    handle: Handle,
    /// The signals of `mask` last seen, for `EDGE`.
    last: AtomicU32,
    /// For a timeline wait, the value it waits for; such a registration
    /// does not watch signals.
    threshold: Option<u64>,
    /// Whether a packet of the registration is queued and unread.
    queued: AtomicBool,
    /// Set once the registration has ended and its watch with it; it is
    /// removed from the list outside the signal hook, so that the hook
    /// never drops anything.
    done: AtomicBool,
    budget: Arc<Budget>,
}

/// Every registration. The lock comes after object locks and before port
/// and scheduler locks.
static REGISTRATIONS: SpinLock<Vec<Arc<Registration>>> = SpinLock::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl Registration {
    /// Queues a packet for `signals` on the port, unless one is queued.
    /// Returns whether the port still exists.
    fn fire(&self, signals: u32) -> bool {
        let mut payload = [0u8; 8];
        payload[0..4].copy_from_slice(&self.mask.to_le_bytes());
        payload[4..8].copy_from_slice(&signals.to_le_bytes());
        self.queue(SIGNAL, &payload)
    }

    /// Queues a packet of `kind` with `payload` on the port, unless one is
    /// queued. Returns whether the port still exists.
    fn queue(&self, kind: u32, payload: &[u8]) -> bool {
        let Some(port) = self.port.upgrade() else {
            return false;
        };
        if self.queued.swap(true, Ordering::AcqRel) {
            return true;
        }
        let packet = Queued {
            bytes: packet(self.key, kind, 0, payload),
            registration: Some(Origin {
                id: self.id,
                process: self.process,
                handle: self.handle,
            }),
            charged: None,
        };
        let mut state = port.state.lock();
        port.push(&mut state, packet);
        drop(state);
        // The port is not the last reference: the caller's handle or a
        // registration's queue holds it while the hook runs.
        drop(port);
        true
    }

    /// Ends the watch of the object, if it still exists and the
    /// registration watches signals.
    fn end_watch(&self) {
        if self.threshold.is_some() {
            return;
        }
        // Upgraded only with no lock held, as dropping the reference may
        // destroy the object.
        if let Some(object) = self.object.upgrade() {
            object.header().unwatch();
        }
    }

    /// Ends the registration's hold on the port's queue and its charge.
    fn release(&self) {
        Self::release_parts(&self.port, &self.budget);
    }

    /// Ends the hold on the queue of `port` and the charge to `budget` of
    /// a registration.
    fn release_parts(port: &Weak<Port>, budget: &Budget) {
        if let Some(port) = port.upgrade() {
            port.state.lock().registrations -= 1;
        }
        budget.release_kernel(REGISTRATION_COST);
    }
}

/// Called by the signal hook for the object of `header`: fires the
/// registrations whose signals are now set. Allocates and drops nothing.
pub fn signals_changed(header: &Header, signals: u32) {
    for r in REGISTRATIONS.lock().iter() {
        if r.koid != header.koid() || r.done.load(Ordering::Acquire) || r.threshold.is_some() {
            continue;
        }
        let now = signals & r.mask;
        let ended = if r.edge {
            let before = r.last.swap(now, Ordering::AcqRel);
            now & !before != 0 && !r.fire(signals)
        } else {
            now != 0 && {
                r.fire(signals);
                true
            }
        };
        if ended {
            r.done.store(true, Ordering::Release);
            header.unwatch();
        }
    }
}

/// Removes the registrations that ended, releasing their charges.
fn purge() {
    let mut ended = Vec::new();
    {
        let mut registrations = REGISTRATIONS.lock();
        let mut i = 0;
        while i < registrations.len() {
            if registrations[i].done.load(Ordering::Acquire) && ended.try_reserve(1).is_ok() {
                ended.push(registrations.swap_remove(i));
            } else {
                i += 1;
            }
        }
    }
    for r in ended {
        r.release();
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
) -> Result<Registration, Error> {
    purge();
    let budget = process.budget().clone();
    {
        let mut state = port.state.lock();
        if state.registrations >= REGISTRATIONS_MAX {
            return Err(Error::NoResources);
        }
        let room = state.registrations + 1;
        state
            .packets
            .try_reserve(room)
            .map_err(|_| Error::NoMemory)?;
        budget.charge_kernel(REGISTRATION_COST)?;
        state.registrations += 1;
    }
    Ok(Registration {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        koid: object.header().koid(),
        mask,
        edge: false,
        key,
        port: Arc::downgrade(port),
        object: Arc::downgrade(object),
        process: Arc::as_ptr(process) as usize,
        handle,
        last: AtomicU32::new(0),
        threshold,
        queued: AtomicBool::new(false),
        done: AtomicBool::new(false),
        budget,
    })
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
    let registration = reserve(object, process, handle, port, key, 0, Some(value))?;
    if current >= value {
        registration.queue(TIMELINE, &current.to_le_bytes());
        registration.release();
        return Ok(());
    }
    let mut registrations = REGISTRATIONS.lock();
    if registrations.try_reserve(1).is_err() {
        drop(registrations);
        registration.release();
        return Err(Error::NoMemory);
    }
    // A registration releases its charge only when told to.
    let (port, budget) = (registration.port.clone(), registration.budget.clone());
    let Ok(registration) = kernel_core::fallible::try_arc(registration) else {
        drop(registrations);
        Registration::release_parts(&port, &budget);
        return Err(Error::NoMemory);
    };
    registrations.push(registration);
    Ok(())
}

/// Queues the packets of the timeline waits on the timeline with object
/// ID `koid` that its new value `value` reaches.
pub fn timeline_raised(koid: u64, value: u64) {
    for r in REGISTRATIONS.lock().iter() {
        let reached = r.threshold.is_some_and(|t| t <= value);
        if r.koid == koid && reached && !r.done.load(Ordering::Acquire) {
            r.queue(TIMELINE, &value.to_le_bytes());
            r.done.store(true, Ordering::Release);
        }
    }
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
    let registration = reserve(object, process, handle, port, key, signals, None)?;
    let header = object.header();
    let registration = Registration {
        edge: options & EDGE != 0,
        ..registration
    };
    let (port, budget) = (registration.port.clone(), registration.budget.clone());
    let Ok(registration) = kernel_core::fallible::try_arc(registration) else {
        Registration::release_parts(&port, &budget);
        return Err(Error::NoMemory);
    };
    let mut registrations = REGISTRATIONS.lock();
    if registrations.try_reserve(1).is_err() {
        drop(registrations);
        registration.release();
        return Err(Error::NoMemory);
    }
    header.watch();
    let now = header.signals() & signals;
    registration.last.store(now, Ordering::Release);
    if now != 0 {
        registration.fire(header.signals());
        if !registration.edge {
            header.unwatch();
            drop(registrations);
            registration.release();
            return Ok(());
        }
    }
    registrations.push(registration);
    Ok(())
}

/// Removes the registrations made through `handle` of the process at
/// `process` without packets, as the handle leaves the table. With
/// `handle` `None`, removes every registration of the process.
pub fn handle_closed(process: usize, handle: Option<Handle>) {
    let mut removed = Vec::new();
    {
        let mut registrations = REGISTRATIONS.lock();
        let mut i = 0;
        while i < registrations.len() {
            let r = &registrations[i];
            if r.process == process && handle.is_none_or(|h| h == r.handle) {
                let r = registrations.swap_remove(i);
                if removed.try_reserve(1).is_ok() {
                    removed.push(r);
                } else {
                    r.release();
                }
            } else {
                i += 1;
            }
        }
    }
    for r in removed {
        if !r.done.load(Ordering::Acquire) {
            r.end_watch();
        }
        r.release();
    }
    purge();
}

/// Removes the registrations made on `port` through `handle` with `key`,
/// with their unread packets, as `port_cancel` does.
pub fn cancel(port: &Arc<Port>, process: usize, handle: Handle, key: u64) {
    let mut removed = Vec::new();
    {
        let mut registrations = REGISTRATIONS.lock();
        let mut i = 0;
        while i < registrations.len() {
            let r = &registrations[i];
            let ours = r.port.as_ptr() == Arc::as_ptr(port);
            if ours && r.process == process && r.handle == handle && r.key == key {
                let r = registrations.swap_remove(i);
                if removed.try_reserve(1).is_ok() {
                    removed.push(r);
                } else {
                    r.release();
                }
            } else {
                i += 1;
            }
        }
    }
    // Packets of one-shot registrations that already ended go too.
    port.state.lock().packets.retain(|p| {
        let packet_key = u64::from_le_bytes(p.bytes[0x00..0x08].try_into().expect("8 bytes"));
        p.registration
            .is_none_or(|o| !(o.process == process && o.handle == handle && packet_key == key))
    });
    for r in removed {
        if !r.done.load(Ordering::Acquire) {
            r.end_watch();
        }
        r.release();
    }
    purge();
}
