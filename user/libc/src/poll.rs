//! `poll`: waiting until descriptors are ready. Each stream among the
//! descriptors gets an asynchronous wait on a port of the call's own,
//! for the signals that the requested events need, and the call waits
//! on that port, and cancels the waits that did not end. Files and
//! directories are always ready.

use crate::errno::{self, EBADF, EINVAL};
use crate::fd::{self, Description};
use alloc::sync::Arc;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const READABLE: u32 = 1 << 0;
const WRITABLE: u32 = 1 << 1;
const PEER_CLOSED: u32 = 1 << 2;
const TIMED_OUT: sys::Status = -21;
const FOREVER: i64 = i64::MAX;
/// The size of a port packet, and where a signal packet holds the
/// signals it observed.
const PACKET_SIZE: usize = 48;
const OBSERVED: usize = 20;

pub const POLLIN: i16 = 0x001;
pub const POLLPRI: i16 = 0x002;
pub const POLLERR: i16 = 0x008;
pub const POLLOUT: i16 = 0x004;
pub const POLLHUP: i16 = 0x010;
pub const POLLNVAL: i16 = 0x020;
pub const POLLRDNORM: i16 = 0x040;
pub const POLLWRNORM: i16 = 0x100;
/// The events that ask for input, and for output.
const INPUT: i16 = POLLIN | POLLRDNORM;
const OUTPUT: i16 = POLLOUT | POLLWRNORM;
/// The most descriptors one call takes.
const NFDS_MAX: u64 = fd::FDS_MAX as u64;

#[repr(C)]
pub struct Pollfd {
    fd: i32,
    events: i16,
    revents: i16,
}

/// The signals that `events` waits for on a stream. A closed peer ends
/// every wait.
fn signals(events: i16) -> u32 {
    let mut signals = PEER_CLOSED;
    if events & INPUT != 0 {
        signals |= READABLE;
    }
    if events & OUTPUT != 0 {
        signals |= WRITABLE;
    }
    signals
}

/// The events that `observed` signals on a stream report for `events`.
fn stream_events(events: i16, observed: u32) -> i16 {
    let mut revents = 0;
    if observed & READABLE != 0 {
        revents |= events & INPUT;
    }
    if observed & WRITABLE != 0 {
        revents |= events & OUTPUT;
    }
    if observed & PEER_CLOSED != 0 {
        revents |= POLLHUP;
    }
    revents
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

pub fn poll(fds: *mut Pollfd, nfds: u64, timeout_ms: i64) -> i64 {
    if nfds > NFDS_MAX {
        return -EINVAL;
    }
    let deadline = match deadline(timeout_ms) {
        Ok(deadline) => deadline,
        Err(e) => return -e,
    };
    let fds: &mut [Pollfd] = if nfds == 0 {
        &mut []
    } else {
        // SAFETY: musl passes an array of `nfds` entries.
        unsafe { core::slice::from_raw_parts_mut(fds, nfds as usize) }
    };
    wait(fds, deadline, None)
}

/// `ppoll`: `poll` with a timeout of `ts`, none for null, and while it
/// waits the signal mask at `mask`, if it is not null.
pub fn ppoll(
    fds: *mut Pollfd,
    nfds: u64,
    ts: *const crate::clock::Timespec,
    mask: *const u64,
) -> i64 {
    // SAFETY: musl passes a mask or null.
    let mask = unsafe { mask.as_ref() }.copied();
    if nfds > NFDS_MAX {
        return -EINVAL;
    }
    let deadline = match timespec_deadline(ts) {
        Ok(deadline) => deadline,
        Err(e) => return -e,
    };
    let fds: &mut [Pollfd] = if nfds == 0 {
        &mut []
    } else {
        // SAFETY: musl passes an array of `nfds` entries.
        unsafe { core::slice::from_raw_parts_mut(fds, nfds as usize) }
    };
    wait(fds, deadline, mask)
}

/// The deadline `ts` from now, none for null.
fn timespec_deadline(ts: *const crate::clock::Timespec) -> Result<i64, i64> {
    if ts.is_null() {
        return Ok(FOREVER);
    }
    let ns = crate::clock::nanos(ts)?;
    Ok(crate::clock::monotonic()?.saturating_add(ns))
}

/// The bits of a descriptor set, as `fd_set` holds them.
const SET_WORDS: usize = 1024 / 64;
type FdSet = [u64; SET_WORDS];

/// `select`, and `pselect6`: waits, with the signal mask `mask` if any,
/// until one of the first `n` descriptors in `read`, `write` and
/// `except`, any of which may be null, is ready, or until `deadline`,
/// keeps only the ready ones in the sets and returns how many bits it
/// kept. A descriptor that is not open fails the call with `EBADF`.
fn select_until(
    n: i64,
    sets: [*mut FdSet; 3],
    deadline: Result<i64, i64>,
    mask: Option<u64>,
) -> i64 {
    let Some(n) = usize::try_from(n).ok().filter(|&n| n <= SET_WORDS * 64) else {
        return -EINVAL;
    };
    let deadline = match deadline {
        Ok(deadline) => deadline,
        Err(e) => return -e,
    };
    // SAFETY: musl passes `fd_set`s or null.
    let mut sets = sets.map(|s| unsafe { s.as_mut() });
    let events = [INPUT, OUTPUT, POLLPRI];
    let has = |set: &Option<&mut FdSet>, fd: usize| {
        set.as_ref()
            .is_some_and(|s| s[fd / 64] >> (fd % 64) & 1 != 0)
    };
    let mut fds = Vec::new();
    for fd in 0..n {
        let wanted = (0..3)
            .filter(|&i| has(&sets[i], fd))
            .fold(0, |e, i| e | events[i]);
        if wanted != 0 {
            fds.push(Pollfd {
                fd: fd as i32,
                events: wanted,
                revents: 0,
            });
        }
    }
    let result = wait(&mut fds, deadline, mask);
    if result < 0 {
        return result;
    }
    if fds.iter().any(|f| f.revents & POLLNVAL != 0) {
        return -EBADF;
    }
    for set in sets.iter_mut().flatten() {
        **set = [0; SET_WORDS];
    }
    // A hangup or an error makes a descriptor ready for reading and
    // writing, as a read or write then does not block.
    let ready = [
        INPUT | POLLHUP | POLLERR,
        OUTPUT | POLLHUP | POLLERR,
        POLLPRI,
    ];
    let mut count = 0;
    for f in &fds {
        let fd = f.fd as usize;
        for (i, set) in sets.iter_mut().enumerate() {
            if let Some(set) = set
                && f.events & events[i] != 0
                && f.revents & ready[i] != 0
            {
                set[fd / 64] |= 1 << (fd % 64);
                count += 1;
            }
        }
    }
    count
}

/// A `struct timeval`.
#[repr(C)]
pub struct Timeval {
    sec: i64,
    usec: i64,
}

pub fn select(n: i64, r: *mut FdSet, w: *mut FdSet, e: *mut FdSet, tv: *const Timeval) -> i64 {
    // SAFETY: musl passes a `struct timeval` or null.
    let deadline = match unsafe { tv.as_ref() } {
        None => Ok(FOREVER),
        Some(t) if t.sec < 0 || !(0..1_000_000).contains(&t.usec) => Err(EINVAL),
        Some(t) => crate::clock::monotonic().map(|now| {
            now.saturating_add(
                t.sec
                    .saturating_mul(1_000_000_000)
                    .saturating_add(t.usec * 1000),
            )
        }),
    };
    select_until(n, [r, w, e], deadline, None)
}

/// `pselect6`: `select` with a timeout of `ts`. `data` points at the
/// signal mask to wait with, or null, and its size.
pub fn pselect6(
    n: i64,
    r: *mut FdSet,
    w: *mut FdSet,
    e: *mut FdSet,
    ts: *const crate::clock::Timespec,
    data: *const [u64; 2],
) -> i64 {
    // SAFETY: musl passes the mask's address and size, or null, and the
    // mask at that address.
    let mask = unsafe { data.as_ref() }
        .filter(|d| d[0] != 0)
        .map(|d| unsafe { *(d[0] as *const u64) });
    select_until(n, [r, w, e], timespec_deadline(ts), mask)
}

/// Waits until one of `fds` is ready or `deadline` passed, as `poll`
/// does, with the signal mask `mask` while it waits, if any: a temporary
/// mask given just before the wait, so that the change and the wait are
/// one step.
fn wait(fds: &mut [Pollfd], deadline: i64, mut mask: Option<u64>) -> i64 {
    let mut port = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::port_create(0, &mut port) };
    if status != mikros_rt::OK {
        return -errno::from_status(status);
    }
    // SAFETY: the port was just created for this call.
    let mut waits = Waits {
        port: unsafe { OwnedHandle::from_raw(port) },
        streams: Vec::new(),
    };
    let mut ready = false;
    for (i, entry) in fds.iter_mut().enumerate() {
        entry.revents = 0;
        if entry.fd < 0 {
            continue;
        }
        let Some(description) = fd::get(entry.fd.into()) else {
            entry.revents = POLLNVAL;
            ready = true;
            continue;
        };
        match &*description {
            Description::Stream(stream) => {
                let key = i as u64 + 1;
                // SAFETY: the call takes no pointers.
                let status = unsafe {
                    sys::object_wait_async(
                        stream.raw(),
                        waits.port.raw(),
                        key,
                        signals(entry.events),
                        0,
                    )
                };
                if status != mikros_rt::OK {
                    return -errno::from_status(status);
                }
                waits.streams.push((description.clone(), key, true));
            }
            Description::File(..) | Description::Directory(..) => {
                entry.revents = entry.events & (INPUT | OUTPUT);
                ready |= entry.revents != 0;
            }
            // An epoll instance is not waited on.
            Description::Epoll(_) => {
                entry.revents = POLLNVAL;
                ready = true;
            }
        }
    }
    // The first packet is waited for until the deadline, unless a
    // descriptor is ready already; those that arrived with it are taken
    // without waiting.
    let mut until = if ready { 0 } else { deadline };
    loop {
        let mut packet = [0u8; PACKET_SIZE];
        if let Some(mask) = mask.take() {
            // SAFETY: the call takes no pointers.
            unsafe { sys::signal_set_wait_mask(mask) };
        }
        // SAFETY: the pointer refers to a whole packet.
        let status = unsafe { sys::port_wait(waits.port.raw(), until, packet.as_mut_ptr()) };
        match status {
            mikros_rt::OK => {
                let key = u64::from_le_bytes(packet[..8].try_into().expect("8 bytes"));
                let observed =
                    u32::from_le_bytes(packet[OBSERVED..OBSERVED + 4].try_into().expect("4 bytes"));
                if let Some(entry) = (key as usize).checked_sub(1).and_then(|i| fds.get_mut(i)) {
                    entry.revents |= stream_events(entry.events, observed);
                }
                waits.ended(key);
                until = 0;
            }
            TIMED_OUT => break,
            status => return -errno::from_status(status),
        }
    }
    drop(waits);
    fds.iter().filter(|e| e.revents != 0).count() as i64
}

/// The call's port and the streams it waits on, with their keys and
/// whether each wait is still registered. The waits that did not end
/// are cancelled before the port is closed.
struct Waits {
    port: OwnedHandle,
    streams: Vec<(Arc<Description>, u64, bool)>,
}

impl Waits {
    /// Records that the wait of `key` ended with its packet.
    fn ended(&mut self, key: u64) {
        for (_, k, pending) in &mut self.streams {
            if *k == key {
                *pending = false;
            }
        }
    }
}

impl Drop for Waits {
    fn drop(&mut self) {
        for (description, key, pending) in &self.streams {
            if let (Description::Stream(stream), true) = (&**description, *pending) {
                // SAFETY: the call takes no pointers. A wait that ended
                // meanwhile has nothing to cancel.
                unsafe { sys::port_cancel(self.port.raw(), stream.raw(), *key) };
            }
        }
    }
}
