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
    registration: Option<u64>,
    charged: Option<Arc<Budget>>,
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
        Ok(Arc::new(Port {
            header: Header::new(ObjectType::Port),
            budget: budget.clone(),
            state: SpinLock::new(State {
                packets: VecDeque::new(),
                registrations: 0,
                users: 0,
                waiters: Vec::new(),
            }),
        }))
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

    /// Removes the oldest packet, waiting until `deadline` for one.
    pub fn wait(&self, deadline: i64) -> Result<[u8; PACKET_SIZE], Error> {
        let thread = sched::current().ok_or(Error::BadState)?;
        let process = thread.process().cloned().ok_or(Error::BadState)?;
        {
            let mut state = self.state.lock();
            state.waiters.try_reserve(1).map_err(|_| Error::NoMemory)?;
            state.waiters.push(thread.clone());
        }
        let result = loop {
            if let Some(p) = self.take() {
                break Ok(p);
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

    /// Removes the oldest packet.
    fn take(&self) -> Option<[u8; PACKET_SIZE]> {
        let mut state = self.state.lock();
        let p = state.packets.pop_front()?;
        if p.registration.is_none() {
            state.users -= 1;
        }
        drop(state);
        if let Some(id) = p.registration {
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
        let Some(port) = self.port.upgrade() else {
            return false;
        };
        if self.queued.swap(true, Ordering::AcqRel) {
            return true;
        }
        let mut payload = [0u8; 8];
        payload[0..4].copy_from_slice(&self.mask.to_le_bytes());
        payload[4..8].copy_from_slice(&signals.to_le_bytes());
        let packet = Queued {
            bytes: packet(self.key, SIGNAL, 0, &payload),
            registration: Some(self.id),
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

    /// Ends the watch of the object, if it still exists.
    fn end_watch(&self) {
        // Upgraded only with no lock held, as dropping the reference may
        // destroy the object.
        if let Some(object) = self.object.upgrade() {
            object.header().unwatch();
        }
    }

    /// Ends the registration's hold on the port's queue and its charge.
    fn release(&self) {
        if let Some(port) = self.port.upgrade() {
            port.state.lock().registrations -= 1;
        }
        self.budget.release_kernel(REGISTRATION_COST);
    }
}

/// Called by the signal hook for the object of `header`: fires the
/// registrations whose signals are now set. Allocates and drops nothing.
pub fn signals_changed(header: &Header, signals: u32) {
    for r in REGISTRATIONS.lock().iter() {
        if r.koid != header.koid() || r.done.load(Ordering::Acquire) {
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
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let header = object.header();
    let registration = Arc::new(Registration {
        id,
        koid: header.koid(),
        mask: signals,
        edge: options & EDGE != 0,
        key,
        port: Arc::downgrade(port),
        object: Arc::downgrade(object),
        process: Arc::as_ptr(process) as usize,
        handle,
        last: AtomicU32::new(0),
        queued: AtomicBool::new(false),
        done: AtomicBool::new(false),
        budget,
    });
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
    port.state.lock().packets.retain(|p| {
        p.registration
            .is_none_or(|id| !removed.iter().any(|r| r.id == id))
    });
    for r in removed {
        if !r.done.load(Ordering::Acquire) {
            r.end_watch();
        }
        r.release();
    }
    purge();
}
