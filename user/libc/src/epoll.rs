//! epoll. Each instance is a port, on which every descriptor in its
//! interest list has an `EDGE` registration for the signals its events
//! need: the kernel queues a packet when one of them becomes set, and once
//! at registration when one is set already. An entry keeps the signals
//! its last packet or look reported. A level-triggered entry is reported
//! at every wait for as long as a look at its object finds it ready; an
//! edge-triggered one once per packet.
//!
//! An instance stays in the process: it is never passed to another one.

use crate::errno::{self, EBADF, EEXIST, EFAULT, EINVAL, ENOENT, EPERM};
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
pub const EPOLLET: u32 = 1 << 31;

/// The events and flags that `epoll_ctl` takes.
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
    | EPOLLET;
const INPUT: u32 = EPOLLIN | EPOLLRDNORM;
const OUTPUT: u32 = EPOLLOUT | EPOLLWRNORM;

const EPOLL_CTL_ADD: i64 = 1;
const EPOLL_CTL_DEL: i64 = 2;
const EPOLL_CTL_MOD: i64 = 3;

const READABLE: u32 = 1 << 0;
const WRITABLE: u32 = 1 << 1;
const PEER_CLOSED: u32 = 1 << 2;
/// The `object_wait_async` option that keeps a registration.
const EDGE: u32 = 1 << 0;
const TIMED_OUT: sys::Status = -21;
const FOREVER: i64 = i64::MAX;
const PACKET_SIZE: usize = 48;
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
    /// The events asked for, with `EPOLLET`.
    events: u32,
    data: u64,
    /// The signals the last packet or look reported.
    observed: u32,
}

impl Entry {
    /// Whether the entry is of descriptor `fd` and `description`.
    fn is(&self, fd: i32, description: &Arc<Description>) -> bool {
        self.fd == fd && core::ptr::eq(self.description.as_ptr(), Arc::as_ptr(description))
    }
}

/// The signals that `events` waits for. A closed peer always counts.
fn signals(events: u32) -> u32 {
    let mut signals = PEER_CLOSED;
    if events & INPUT != 0 {
        signals |= READABLE;
    }
    if events & OUTPUT != 0 {
        signals |= WRITABLE;
    }
    signals
}

/// The events that `observed` signals report for `events`.
fn reported(events: u32, observed: u32) -> u32 {
    let mut out = 0;
    if observed & READABLE != 0 {
        out |= events & INPUT;
    }
    if observed & WRITABLE != 0 {
        out |= events & OUTPUT;
    }
    if observed & PEER_CLOSED != 0 {
        out |= EPOLLHUP | events & EPOLLRDHUP;
    }
    out
}

/// The stream that `description` waits on, or why it cannot be watched.
fn watched(description: &Description) -> Result<&OwnedHandle, i64> {
    match description {
        Description::Stream(stream) => Ok(stream),
        Description::File(..) | Description::Directory(..) | Description::Epoll(_) => Err(EPERM),
    }
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
    let handle = watched(&target)?.raw();
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
            unsafe { sys::port_cancel(epoll.port.raw(), handle, old) };
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
    // SAFETY: the call takes no pointers.
    let status = unsafe {
        sys::object_wait_async(handle, epoll.port.raw(), key, signals(event.events), EDGE)
    };
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

/// The key and observed signals of the next packet on the port, waiting
/// for one until `deadline`, or none once it passed.
fn take(epoll: &Epoll, deadline: i64) -> Result<Option<(u64, u32)>, i64> {
    let mut packet = [0u8; PACKET_SIZE];
    // SAFETY: the pointer refers to a whole packet.
    let status = unsafe { sys::port_wait(epoll.port.raw(), deadline, packet.as_mut_ptr()) };
    match status {
        mikros_rt::OK => {
            let key = u64::from_le_bytes(packet[..8].try_into().expect("8 bytes"));
            let observed = &packet[OBSERVED..OBSERVED + 4];
            let observed = u32::from_le_bytes(observed.try_into().expect("4 bytes"));
            Ok(Some((key, observed)))
        }
        TIMED_OUT => Ok(None),
        // An interruption is EINTR, as for every wait.
        status => Err(errno::from_status(status)),
    }
}

/// Records the signals of `packet` for the entry it belongs to, if it is
/// still in the list.
fn note(epoll: &Epoll, (key, observed): (u64, u32)) {
    let mut state = epoll.state.lock();
    if let Some(entry) = state.entries.iter_mut().find(|e| e.key == key) {
        entry.observed = observed;
    }
}

/// Stores the events of the ready entries in `out`, as many as fit, and
/// returns how many. A level-triggered entry is looked at again first,
/// and stays ready while it is; an edge-triggered one is reported once.
/// Reported entries go to the end of the list, so that every ready one
/// gets its turn.
fn collect(epoll: &Epoll, out: &mut [Event]) -> usize {
    let mut state = epoll.state.lock();
    state.entries.retain(|e| e.description.strong_count() > 0);
    let mut n = 0;
    let mut reported = Vec::new();
    for (i, entry) in state.entries.iter_mut().enumerate() {
        if n == out.len() {
            break;
        }
        let edge = entry.events & EPOLLET != 0;
        if !edge && self::reported(entry.events, entry.observed) != 0 {
            entry.observed = look(entry, signals(entry.events));
        }
        let events = self::reported(entry.events, entry.observed);
        if events == 0 {
            continue;
        }
        if edge {
            entry.observed = 0;
        }
        out[n] = Event {
            events,
            data: entry.data,
        };
        n += 1;
        reported.push(i);
    }
    // The reported entries move to the end, in the order they were in.
    for (moved, i) in reported.into_iter().enumerate() {
        let entry = state.entries.remove(i - moved);
        state.entries.push(entry);
    }
    n
}

/// The signals of `signals` that the object of `entry` has set now.
fn look(entry: &Entry, signals: u32) -> u32 {
    let Some(description) = entry.description.upgrade() else {
        return 0;
    };
    let Ok(handle) = watched(&description) else {
        return 0;
    };
    let mut observed = 0;
    // SAFETY: the out pointer is valid for the call; the deadline has
    // passed, so it reports the signals that are set.
    unsafe { sys::object_wait_one(handle.raw(), signals, 0, &mut observed) };
    observed & signals
}
