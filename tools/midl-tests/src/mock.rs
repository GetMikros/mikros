//! An in-memory kernel for the generated bindings: handles, events and
//! channel endpoints, shared by every thread of the test process. Only
//! what the bindings' runtime calls exists.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, MutexGuard};

pub const SHOULD_WAIT: i32 = -22;
pub const PEER_CLOSED: i32 = -24;
const BAD_HANDLE: i32 = -11;
const ACCESS_DENIED: i32 = -30;

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

#[derive(Default)]
struct Endpoint {
    handles: usize,
}

#[derive(Default)]
struct State {
    next_handle: u32,
    next_object: u64,
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
        if e.handles == 0 {
            self.endpoints.remove(&entry.object);
        }
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
    s.endpoints.insert(a, Endpoint::default());
    s.endpoints.insert(b, Endpoint::default());
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
