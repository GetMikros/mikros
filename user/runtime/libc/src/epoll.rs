//! epoll. Each instance is a port, on which every descriptor in its
//! interest list has an `EDGE` registration for the signals its events
//! need: the kernel queues a packet when one of them becomes set, and once
//! at registration when one is set already. An entry keeps the signals
//! its last packet or look reported. A level-triggered entry is reported
//! at every wait for as long as a look at its object finds it ready; an
//! edge-triggered one once per packet, and a one-shot one once until it
//! is changed.
//!
//! The port is `READABLE` while a packet is queued on it, which makes
//! the instance ready for `poll` and other instances. An entry that is
//! still ready after a wait gets a user packet with its key, so that the
//! port stays readable for as long as a wait would report an event.
//!
//! An instance stays in the process: it is never passed to another one.

use crate::errno::{self, EBADF, EEXIST, EFAULT, EINVAL, ELOOP, ENOENT, EPERM};
use crate::fd::{self, Description};
use crate::lock::Lock;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

pub const EPOLLIN: u32 = 0x001;
pub const EPOLLPRI: u32 = 0x002;
pub const EPOLLOUT: u32 = 0x004;
pub const EPOLLERR: u32 = 0x008;
pub const EPOLLHUP: u32 = 0x010;
pub const EPOLLRDNORM: u32 = 0x040;
pub const EPOLLRDBAND: u32 = 0x080;
pub const EPOLLWRNORM: u32 = 0x100;
pub const EPOLLWRBAND: u32 = 0x200;
pub const EPOLLMSG: u32 = 0x400;
pub const EPOLLRDHUP: u32 = 0x2000;
pub const EPOLLEXCLUSIVE: u32 = 1 << 28;
pub const EPOLLWAKEUP: u32 = 1 << 29;
pub const EPOLLONESHOT: u32 = 1 << 30;
pub const EPOLLET: u32 = 1 << 31;

/// The events and flags that `epoll_ctl` takes. `EPOLLEXCLUSIVE` and
/// `EPOLLWAKEUP` are ignored, and `EPOLLPRI` is never reported.
const KNOWN: u32 = EPOLLIN
    | EPOLLPRI
    | EPOLLOUT
    | EPOLLERR
    | EPOLLHUP
    | EPOLLRDNORM
    | EPOLLRDBAND
    | EPOLLWRNORM
    | EPOLLWRBAND
    | EPOLLMSG
    | EPOLLRDHUP
    | EPOLLEXCLUSIVE
    | EPOLLWAKEUP
    | EPOLLONESHOT
    | EPOLLET;
const INPUT: u32 = EPOLLIN | EPOLLRDNORM;
const OUTPUT: u32 = EPOLLOUT | EPOLLWRNORM;

const EPOLL_CTL_ADD: i64 = 1;
const EPOLL_CTL_DEL: i64 = 2;
const EPOLL_CTL_MOD: i64 = 3;
/// The deepest that instances may watch each other, as on Linux.
const NESTS_MAX: usize = 4;

const READABLE: u32 = 1 << 0;
const WRITABLE: u32 = 1 << 1;
const PEER_CLOSED: u32 = 1 << 2;
/// The `object_wait_async` option that keeps a registration.
const EDGE: u32 = 1 << 0;
const TIMED_OUT: sys::Status = -21;
const FOREVER: i64 = i64::MAX;
const PACKET_SIZE: usize = 48;
/// The packet type of `SIGNAL` packets.
const SIGNAL: u32 = 1;
/// Where a `SIGNAL` packet holds the signals it observed.
const OBSERVED: usize = 20;

/// `struct epoll_event`, which is packed on x86_64.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Event {
    events: u32,
    data: u64,
}

/// An epoll instance.
pub struct Epoll {
    port: OwnedHandle,
    state: Lock<State>,
}

impl Epoll {
    /// The port, which is `READABLE` while the instance would report an
    /// event.
    pub fn port(&self) -> &OwnedHandle {
        &self.port
    }
}

struct State {
    entries: Vec<Entry>,
    next_key: u64,
}

/// A descriptor in the interest list. The entry is the descriptor's
/// number together with its open description, as on Linux, and goes once
/// the description is closed.
struct Entry {
    fd: i32,
    description: Weak<Description>,
    key: u64,
    /// The events asked for, with the flags.
    events: u32,
    data: u64,
    /// The signals the last packet or look reported.
    observed: u32,
    /// Set once a one-shot entry reported its event, until it is changed.
    spent: bool,
}

impl Entry {
    /// Whether the entry is of descriptor `fd` and `description`.
    fn is(&self, fd: i32, description: &Arc<Description>) -> bool {
        self.fd == fd && core::ptr::eq(self.description.as_ptr(), Arc::as_ptr(description))
    }
}

/// What kind of object a watched descriptor is waited on through.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Stream,
    /// Another instance's port.
    Port,
    /// A timer descriptor's timer, which is signaled once it expired.
    Timer,
    /// A terminal's readiness event, with `USER0` for input and `USER1`
    /// for output.
    Terminal,
}

/// What a watched descriptor is waited on through: its object, and what
/// kind of object that is.
struct Watched<'a> {
    handle: sys::Handle,
    _description: core::marker::PhantomData<&'a Description>,
    kind: Kind,
}

/// The object that `description` is waited on through, or why it cannot
/// be watched: files and directories are not, as on Linux.
fn watched(description: &Description) -> Result<Watched<'_>, i64> {
    match description {
        Description::Stream(stream) => Ok(Watched {
            handle: stream.raw(),
            _description: core::marker::PhantomData,
            kind: Kind::Stream,
        }),
        Description::Epoll(epoll) => Ok(Watched {
            handle: epoll.port.raw(),
            _description: core::marker::PhantomData,
            kind: Kind::Port,
        }),
        Description::Timer(timer) => Ok(Watched {
            handle: timer.timer.raw(),
            _description: core::marker::PhantomData,
            kind: Kind::Timer,
        }),
        // The process's readiness event for the terminal, which lives as
        // long as the description.
        Description::Terminal(t) => Ok(Watched {
            handle: t.readiness()?,
            _description: core::marker::PhantomData,
            kind: Kind::Terminal,
        }),
        Description::File(..) | Description::Directory(..) | Description::Folder(_) => Err(EPERM),
    }
}

/// The signals that `events` waits for on `watched`. A closed peer always
/// counts for a stream; an instance only becomes readable, and a timer
/// descriptor only readable once its timer expired.
fn signals(watched: &Watched, events: u32) -> u32 {
    let input = events & INPUT != 0;
    match watched.kind {
        Kind::Stream => {
            let mut signals = PEER_CLOSED;
            if input {
                signals |= READABLE;
            }
            if events & OUTPUT != 0 {
                signals |= WRITABLE;
            }
            signals
        }
        Kind::Port if input => READABLE,
        Kind::Timer if input => crate::timerfd::SIGNALED,
        Kind::Port | Kind::Timer => 0,
        Kind::Terminal => {
            let mut signals = 0;
            if input {
                signals |= crate::terminal::USER0;
            }
            if events & OUTPUT != 0 {
                signals |= crate::terminal::USER1;
            }
            signals
        }
    }
}

/// The events that `observed` signals report for `events`.
fn reported(events: u32, observed: u32) -> u32 {
    let mut out = 0;
    if observed & (READABLE | crate::terminal::USER0) != 0 {
        out |= events & INPUT;
    }
    if observed & (WRITABLE | crate::terminal::USER1) != 0 {
        out |= events & OUTPUT;
    }
    if observed & PEER_CLOSED != 0 {
        out |= EPOLLHUP | events & EPOLLRDHUP;
    }
    // Only a timer descriptor's timer is waited on for this signal.
    if observed & crate::timerfd::SIGNALED != 0 {
        out |= events & INPUT;
    }
    out
}

/// `epoll_create1`: a new instance, which closes on `exec` with
/// `EPOLL_CLOEXEC`.
pub fn create(flags: i64) -> i64 {
    let cloexec = i64::from(crate::fs::O_CLOEXEC);
    if flags & !cloexec != 0 {
        return -EINVAL;
    }
    let mut port = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::port_create(0, &mut port) };
    if status != mikros_rt::OK {
        return -errno::from_status(status);
    }
    let epoll = Epoll {
        // SAFETY: the kernel just created the handle for us.
        port: unsafe { OwnedHandle::from_raw(port) },
        state: Lock::new(State {
            entries: Vec::new(),
            next_key: 1,
        }),
    };
    fd::insert(Arc::new(Description::Epoll(epoll)), flags & cloexec != 0, 0)
}

/// `epoll_create`: as `epoll_create1` without flags, for a positive
/// `size`, which is otherwise unused.
pub fn create_sized(size: i64) -> i64 {
    if size <= 0 {
        return -EINVAL;
    }
    create(0)
}

/// The instance of descriptor `epfd`.
fn instance(epfd: i64) -> Result<Arc<Description>, i64> {
    let description = fd::get(epfd).ok_or(EBADF)?;
    match &*description {
        Description::Epoll(_) => Ok(description),
        _ => Err(EINVAL),
    }
}

/// The instances that the instance `epoll` watches.
fn watched_instances(epoll: &Epoll) -> Vec<Arc<Description>> {
    let state = epoll.state.lock();
    state
        .entries
        .iter()
        .filter_map(|e| e.description.upgrade())
        .filter(|d| matches!(**d, Description::Epoll(_)))
        .collect()
}

/// Whether watching the instance `target` from the instance `from` would
/// make instances watch each other in a loop, or more deeply than
/// `NESTS_MAX`, as `depth` instances watch `from` already.
fn loops(from: &Arc<Description>, target: &Arc<Description>, depth: usize) -> bool {
    if Arc::ptr_eq(from, target) || depth >= NESTS_MAX {
        return true;
    }
    let Description::Epoll(epoll) = &**target else {
        return false;
    };
    watched_instances(epoll)
        .iter()
        .any(|inner| loops(from, inner, depth + 1))
}

/// `epoll_ctl`: adds descriptor `fd` to the interest list of `epfd` with
/// the event at `event`, changes its event, or removes it.
pub fn ctl(epfd: i64, op: i64, fd: i64, event: *const Event) -> i64 {
    match control(epfd, op, fd, event) {
        Ok(()) => 0,
        Err(e) => -e,
    }
}

fn control(epfd: i64, op: i64, fd: i64, event: *const Event) -> Result<(), i64> {
    let instance = instance(epfd)?;
    let Description::Epoll(epoll) = &*instance else {
        unreachable!("an instance's description");
    };
    let target = fd::get(fd).ok_or(EBADF)?;
    if fd == epfd {
        return Err(EINVAL);
    }
    let fd = fd as i32;
    let watched = watched(&target)?;
    let wanted = match op {
        EPOLL_CTL_ADD | EPOLL_CTL_MOD => {
            if event.is_null() {
                return Err(EFAULT);
            }
            // SAFETY: musl passes the caller's event, which may be
            // unaligned.
            let event = unsafe { event.read_unaligned() };
            if event.events & !KNOWN != 0 {
                return Err(EINVAL);
            }
            Some(event)
        }
        EPOLL_CTL_DEL => None,
        _ => return Err(EINVAL),
    };
    if op == EPOLL_CTL_ADD && watched.kind == Kind::Port && loops(&instance, &target, 0) {
        return Err(ELOOP);
    }
    let mut state = epoll.state.lock();
    // Entries whose description closed are gone.
    state.entries.retain(|e| e.description.strong_count() > 0);
    let at = state.entries.iter().position(|e| e.is(fd, &target));
    let at = match (op, at) {
        (EPOLL_CTL_ADD, Some(_)) => return Err(EEXIST),
        (EPOLL_CTL_ADD, None) => None,
        (_, None) => return Err(ENOENT),
        (_, Some(at)) => {
            let old = state.entries[at].key;
            // SAFETY: the call takes no pointers. A registration that
            // ended has nothing to cancel.
            unsafe { sys::port_cancel(epoll.port.raw(), watched.handle, old) };
            Some(at)
        }
    };
    let Some(event) = wanted else {
        if let Some(at) = at {
            state.entries.swap_remove(at);
        }
        return Ok(());
    };
    let key = state.next_key;
    state.next_key += 1;
    let signals = signals(&watched, event.events);
    // SAFETY: the call takes no pointers.
    let status =
        unsafe { sys::object_wait_async(watched.handle, epoll.port.raw(), key, signals, EDGE) };
    if status != mikros_rt::OK {
        if let Some(at) = at {
            state.entries.swap_remove(at);
        }
        return Err(errno::from_status(status));
    }
    let entry = Entry {
        fd,
        description: Arc::downgrade(&target),
        key,
        events: event.events,
        data: event.data,
        observed: 0,
        spent: false,
    };
    match at {
        Some(at) => state.entries[at] = entry,
        None => state.entries.push(entry),
    }
    Ok(())
}

/// `epoll_pwait`: waits until a descriptor of `epfd` has an event, until
/// `timeout_ms` milliseconds passed, none when it is negative, with the
/// signal mask at `mask` while it waits, if it is not null, and stores
/// at most `max` events at `events`. Returns how many it stored.
pub fn pwait(
    epfd: i64,
    events: *mut Event,
    max: i64,
    timeout_ms: i64,
    mask: *const u64,
    mask_size: u64,
) -> i64 {
    if max <= 0 || max > i64::from(i32::MAX) {
        return -EINVAL;
    }
    if !mask.is_null() && mask_size != 8 {
        return -EINVAL;
    }
    // SAFETY: musl passes a mask or null.
    let mask = unsafe { mask.as_ref() }.copied();
    let instance = match instance(epfd) {
        Ok(instance) => instance,
        Err(e) => return -e,
    };
    let Description::Epoll(epoll) = &*instance else {
        unreachable!("an instance's description");
    };
    let deadline = match deadline(timeout_ms) {
        Ok(deadline) => deadline,
        Err(e) => return -e,
    };
    // SAFETY: musl passes room for `max` events, which may be unaligned.
    let out = unsafe { core::slice::from_raw_parts_mut(events, max as usize) };
    match wait(epoll, out, deadline, mask) {
        Ok(n) => n as i64,
        Err(e) => -e,
    }
}

/// The deadline on the monotonic clock of a timeout of `ms`
/// milliseconds: none when it is negative.
fn deadline(ms: i64) -> Result<i64, i64> {
    if ms < 0 {
        return Ok(FOREVER);
    }
    if ms == 0 {
        return Ok(0);
    }
    let now = crate::clock::monotonic()?;
    Ok(now.saturating_add(ms.saturating_mul(1_000_000)))
}

fn wait(
    epoll: &Epoll,
    out: &mut [Event],
    deadline: i64,
    mut mask: Option<u64>,
) -> Result<usize, i64> {
    loop {
        // The packets already queued are taken first, without waiting.
        while let Some(packet) = take(epoll, 0)? {
            note(epoll, packet);
        }
        let n = collect(epoll, out);
        if n != 0 {
            return Ok(n);
        }
        if let Some(mask) = mask.take() {
            // SAFETY: the call takes no pointers.
            unsafe { sys::signal_set_wait_mask(mask) };
        }
        match take(epoll, deadline)? {
            Some(packet) => note(epoll, packet),
            None => return Ok(0),
        }
    }
}

/// A packet taken from the port: its key, and the signals it observed
/// for a `SIGNAL` packet; a user packet only says to look again.
struct Taken {
    key: u64,
    observed: Option<u32>,
}

/// The next packet on the port, waiting for one until `deadline`, or
/// none once it passed.
fn take(epoll: &Epoll, deadline: i64) -> Result<Option<Taken>, i64> {
    let mut packet = [0u8; PACKET_SIZE];
    // SAFETY: the pointer refers to a whole packet.
    let status = unsafe { sys::port_wait(epoll.port.raw(), deadline, packet.as_mut_ptr()) };
    match status {
        mikros_rt::OK => {
            let word =
                |at: usize| u32::from_le_bytes(packet[at..at + 4].try_into().expect("4 bytes"));
            Ok(Some(Taken {
                key: u64::from_le_bytes(packet[..8].try_into().expect("8 bytes")),
                observed: (word(8) == SIGNAL).then(|| word(OBSERVED)),
            }))
        }
        TIMED_OUT => Ok(None),
        // An interruption is EINTR, as for every wait.
        status => Err(errno::from_status(status)),
    }
}

/// Records the signals of a `SIGNAL` packet for the entry it belongs to,
/// if it is still in the list.
fn note(epoll: &Epoll, taken: Taken) {
    let Some(observed) = taken.observed else {
        return;
    };
    let mut state = epoll.state.lock();
    if let Some(entry) = state.entries.iter_mut().find(|e| e.key == taken.key) {
        entry.observed = observed;
    }
}

/// Stores the events of the ready entries in `out`, as many as fit, and
/// returns how many. A level-triggered entry is looked at again first,
/// and stays ready while it is; an edge-triggered one is reported once,
/// and a one-shot one is then spent. Reported entries go to the end of
/// the list, so that every ready one gets its turn. Each entry that is
/// still ready afterwards gets a user packet on the port, which keeps it
/// readable.
fn collect(epoll: &Epoll, out: &mut [Event]) -> usize {
    let mut state = epoll.state.lock();
    state.entries.retain(|e| e.description.strong_count() > 0);
    let mut n = 0;
    let mut moved = Vec::new();
    for (i, entry) in state.entries.iter_mut().enumerate() {
        if entry.spent {
            continue;
        }
        let edge = entry.events & (EPOLLET | EPOLLONESHOT) != 0;
        if !edge && reported(entry.events, entry.observed) != 0 {
            entry.observed = look(entry);
        }
        let events = reported(entry.events, entry.observed);
        if events == 0 || n == out.len() {
            continue;
        }
        if edge {
            entry.observed = 0;
        }
        entry.spent = entry.events & EPOLLONESHOT != 0;
        out[n] = Event {
            events,
            data: entry.data,
        };
        n += 1;
        moved.push(i);
    }
    // The reported entries move to the end, in the order they were in.
    for (count, i) in moved.into_iter().enumerate() {
        let entry = state.entries.remove(i - count);
        state.entries.push(entry);
    }
    for entry in &state.entries {
        if !entry.spent && reported(entry.events, entry.observed) != 0 {
            let mut packet = [0u8; PACKET_SIZE];
            packet[..8].copy_from_slice(&entry.key.to_le_bytes());
            // SAFETY: the pointer describes a whole packet. A full queue
            // keeps the port readable anyway.
            unsafe { sys::port_queue(epoll.port.raw(), packet.as_ptr()) };
        }
    }
    n
}

/// The signals that the object of `entry` has set now, of those it
/// waits for.
fn look(entry: &Entry) -> u32 {
    let Some(description) = entry.description.upgrade() else {
        return 0;
    };
    let Ok(watched) = watched(&description) else {
        return 0;
    };
    let signals = signals(&watched, entry.events);
    let mut observed = 0;
    // SAFETY: the out pointer is valid for the call; the deadline has
    // passed, so it reports the signals that are set.
    unsafe { sys::object_wait_one(watched.handle, signals, 0, &mut observed) };
    observed & signals
}
