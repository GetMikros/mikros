//! An in-memory kernel for the generated bindings: handles, events and
//! channels, shared by every thread of the test process. Only what the
//! bindings' runtime calls exists.

use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

pub const SHOULD_WAIT: i32 = -22;
pub const PEER_CLOSED: i32 = -24;
const BAD_HANDLE: i32 = -11;
const ACCESS_DENIED: i32 = -30;
const TIMED_OUT: i32 = -21;

pub const EVENT: u32 = 5;
pub const CHANNEL: u32 = 4;
pub const READ: u32 = 1 << 2;
pub const WRITE: u32 = 1 << 3;

/// What a handle refers to, with its rights.
#[derive(Clone, Copy)]
struct Entry {
    object: u64,
    kind: u32,
    rights: u32,
}

struct Message {
    bytes: Vec<u8>,
    handles: Vec<Entry>,
}

#[derive(Default)]
struct Endpoint {
    peer: u64,
    peer_closed: bool,
    handles: usize,
    queue: VecDeque<Message>,
    /// Calls waiting for replies, by transaction ID, and the replies.
    calls: HashMap<u32, Option<Message>>,
}

#[derive(Default)]
struct State {
    next_handle: u32,
    next_object: u64,
    next_txid: u32,
    handles: HashMap<u32, Entry>,
    endpoints: HashMap<u64, Endpoint>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static CHANGED: Condvar = Condvar::new();

fn state() -> MutexGuard<'static, Option<State>> {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(State::default);
    guard
}

impl State {
    fn add(&mut self, entry: Entry) -> u32 {
        self.next_handle += 1;
        let h = self.next_handle;
        if let Some(e) = self.endpoints.get_mut(&entry.object) {
            e.handles += 1;
        }
        self.handles.insert(h, entry);
        h
    }

    fn take(&mut self, h: u32) -> Option<Entry> {
        let entry = self.handles.remove(&h)?;
        if let Some(e) = self.endpoints.get_mut(&entry.object) {
            e.handles -= 1;
        }
        Some(entry)
    }

    /// Drops what an entry that left the handle table keeps alive.
    fn release(&mut self, entry: Entry) {
        let Some(e) = self.endpoints.get(&entry.object) else {
            return;
        };
        let held = e.handles > 0
            || self.endpoints.values().any(|o| {
                o.queue
                    .iter()
                    .any(|m| m.handles.iter().any(|h| h.object == entry.object))
            });
        if !held {
            let peer = e.peer;
            let messages: Vec<Message> = self
                .endpoints
                .remove(&entry.object)
                .map(|e| e.queue.into_iter().collect())
                .unwrap_or_default();
            if let Some(p) = self.endpoints.get_mut(&peer) {
                p.peer_closed = true;
            }
            for m in messages {
                for h in m.handles {
                    self.release(h);
                }
            }
        }
    }

    fn deliver(&mut self, from: u64, message: Message) -> Result<(), i32> {
        let peer = self.endpoints[&from].peer;
        let Some(target) = self.endpoints.get_mut(&peer) else {
            for h in message.handles {
                self.release(h);
            }
            return Err(PEER_CLOSED);
        };
        let txid = u32::from_le_bytes(
            message.bytes[..4]
                .try_into()
                .expect("a message has a header"),
        );
        match target.calls.get_mut(&txid) {
            Some(slot @ None) => *slot = Some(message),
            _ => target.queue.push_back(message),
        }
        Ok(())
    }
}

pub fn handle_close(h: u32) {
    let mut guard = state();
    let s = guard.as_mut().expect("initialized");
    if let Some(entry) = s.take(h) {
        s.release(entry);
    }
    CHANGED.notify_all();
}

pub fn handle_info(h: u32) -> Option<(u32, u32)> {
    let guard = state();
    guard
        .as_ref()
        .expect("initialized")
        .handles
        .get(&h)
        .map(|e| (e.kind, e.rights))
}

pub fn handle_replace(h: u32, rights: u32) -> Result<u32, i32> {
    let mut guard = state();
    let s = guard.as_mut().expect("initialized");
    let entry = s.take(h).ok_or(BAD_HANDLE)?;
    if entry.rights & rights != rights {
        s.release(entry);
        return Err(ACCESS_DENIED);
    }
    Ok(s.add(Entry { rights, ..entry }))
}

pub fn channel_create() -> Result<(u32, u32), i32> {
    let mut guard = state();
    let s = guard.as_mut().expect("initialized");
    let (a, b) = (s.next_object + 1, s.next_object + 2);
    s.next_object += 2;
    s.endpoints.insert(
        a,
        Endpoint {
            peer: b,
            ..Endpoint::default()
        },
    );
    s.endpoints.insert(
        b,
        Endpoint {
            peer: a,
            ..Endpoint::default()
        },
    );
    let rights = READ | WRITE | 0x2 | 0x40;
    Ok((
        s.add(Entry {
            object: a,
            kind: CHANNEL,
            rights,
        }),
        s.add(Entry {
            object: b,
            kind: CHANNEL,
            rights,
        }),
    ))
}

/// An event handle with `rights`, for tests.
pub fn event(rights: u32) -> u32 {
    let mut guard = state();
    let s = guard.as_mut().expect("initialized");
    s.next_object += 1;
    let object = s.next_object;
    s.add(Entry {
        object,
        kind: EVENT,
        rights,
    })
}

/// How many handles are open.
pub fn open_handles() -> usize {
    state().as_ref().expect("initialized").handles.len()
}

fn endpoint_of(s: &State, ch: u32) -> Result<u64, i32> {
    match s.handles.get(&ch) {
        Some(e) if e.kind == CHANNEL => Ok(e.object),
        _ => Err(BAD_HANDLE),
    }
}

fn take_all(s: &mut State, handles: &[u32]) -> Vec<Entry> {
    handles.iter().filter_map(|&h| s.take(h)).collect()
}

pub fn channel_write(ch: u32, bytes: &[u8], handles: &[u32]) -> Result<(), i32> {
    let mut guard = state();
    let s = guard.as_mut().expect("initialized");
    let entries = take_all(s, handles);
    let result = match endpoint_of(s, ch) {
        Ok(from) => s.deliver(
            from,
            Message {
                bytes: bytes.to_vec(),
                handles: entries,
            },
        ),
        Err(e) => {
            for h in entries {
                s.release(h);
            }
            Err(e)
        }
    };
    CHANGED.notify_all();
    result
}

fn received(s: &mut State, message: Message) -> (Vec<u8>, Vec<u32>) {
    let handles = message.handles.into_iter().map(|h| s.add(h)).collect();
    (message.bytes, handles)
}

pub fn channel_read(ch: u32) -> Result<(Vec<u8>, Vec<u32>), i32> {
    let mut guard = state();
    let s = guard.as_mut().expect("initialized");
    let at = endpoint_of(s, ch)?;
    let endpoint = s
        .endpoints
        .get_mut(&at)
        .expect("open handles have endpoints");
    match endpoint.queue.pop_front() {
        Some(message) => Ok(received(s, message)),
        None if endpoint.peer_closed => Err(PEER_CLOSED),
        None => Err(SHOULD_WAIT),
    }
}

pub fn channel_call(
    ch: u32,
    bytes: &[u8],
    handles: &[u32],
    _deadline: i64,
) -> Result<(Vec<u8>, Vec<u32>), i32> {
    let mut guard = state();
    let s = guard.as_mut().expect("initialized");
    let entries = take_all(s, handles);
    let at = endpoint_of(s, ch)?;
    s.next_txid += 1;
    let txid = 0x8000_0000 | s.next_txid;
    let mut bytes = bytes.to_vec();
    bytes[..4].copy_from_slice(&txid.to_le_bytes());
    s.endpoints
        .get_mut(&at)
        .expect("open")
        .calls
        .insert(txid, None);
    s.deliver(
        at,
        Message {
            bytes,
            handles: entries,
        },
    )?;
    CHANGED.notify_all();
    loop {
        let s = guard.as_mut().expect("initialized");
        let endpoint = s.endpoints.get_mut(&at).ok_or(BAD_HANDLE)?;
        if let Some(Some(_)) = endpoint.calls.get(&txid) {
            let reply = endpoint
                .calls
                .remove(&txid)
                .flatten()
                .expect("the reply is there");
            return Ok(received(s, reply));
        }
        if endpoint.peer_closed {
            endpoint.calls.remove(&txid);
            return Err(PEER_CLOSED);
        }
        let (g, timeout) = CHANGED
            .wait_timeout(guard, Duration::from_secs(5))
            .unwrap_or_else(|e| e.into_inner());
        guard = g;
        if timeout.timed_out() {
            return Err(TIMED_OUT);
        }
    }
}

pub fn wait_readable(ch: u32, _deadline: i64) -> Result<(), i32> {
    let mut guard = state();
    loop {
        let s = guard.as_mut().expect("initialized");
        let at = endpoint_of(s, ch)?;
        let endpoint = &s.endpoints[&at];
        if !endpoint.queue.is_empty() || endpoint.peer_closed {
            return Ok(());
        }
        let (g, timeout) = CHANGED
            .wait_timeout(guard, Duration::from_secs(5))
            .unwrap_or_else(|e| e.into_inner());
        guard = g;
        if timeout.timed_out() {
            return Err(TIMED_OUT);
        }
    }
}

/// The mock's reactor. A wait that cannot complete yet blocks for a
/// moment until the mock's state changes, then wakes its task, so that
/// `block_on` polls it again without keeping other threads from the
/// state.
#[derive(Clone, Default)]
pub struct Reactor;

pub fn readable(_reactor: &Reactor, ch: u32) -> impl Future<Output = Result<(), i32>> {
    std::future::poll_fn(move |cx| {
        let mut guard = state();
        let s = guard.as_mut().expect("initialized");
        let at = match endpoint_of(s, ch) {
            Ok(at) => at,
            Err(e) => return Poll::Ready(Err(e)),
        };
        let endpoint = &s.endpoints[&at];
        if !endpoint.queue.is_empty() || endpoint.peer_closed {
            return Poll::Ready(Ok(()));
        }
        let _ = CHANGED
            .wait_timeout(guard, Duration::from_millis(10))
            .unwrap_or_else(|e| e.into_inner());
        cx.waker().wake_by_ref();
        Poll::Pending
    })
}

/// Runs `future` to completion on this thread, polling it again
/// whenever it is pending.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(out) = future.as_mut().poll(&mut cx) {
            return out;
        }
        std::thread::yield_now();
    }
}
