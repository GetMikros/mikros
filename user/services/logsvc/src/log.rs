//! The log that every source adds to and every reader reads.

use crate::store::{Entry, Store};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_rt::interfaces::mikros::log::{Record, Severity};
use mikros_rt::sys;

/// The bytes of records kept in memory.
const MEMORY: usize = 4 << 20;
/// The UTC time of a record while the time of day is not known.
const UTC_UNKNOWN: i64 = 0;
/// The longest message the kernel log takes, in bytes.
const KERNEL_MESSAGE_MAX: usize = 256;

pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

pub fn severity_value(severity: Severity) -> u8 {
    match severity {
        Severity::Debug => 0,
        Severity::Info => 1,
        Severity::Warning => 2,
        Severity::Error => 3,
        Severity::Unknown(value) => value,
    }
}

fn severity(value: u8) -> Severity {
    match value {
        0 => Severity::Debug,
        1 => Severity::Info,
        2 => Severity::Warning,
        3 => Severity::Error,
        value => Severity::Unknown(value),
    }
}

fn record(entry: &Entry) -> Record {
    Record {
        sequence: entry.sequence,
        timestamp: entry.timestamp,
        utc: entry.utc,
        pid: entry.pid,
        app_id: entry.app_id.clone(),
        severity: severity(entry.severity),
        message: entry.message.clone(),
    }
}

pub struct Log {
    store: RefCell<Store>,
    /// The kernel log that the records of clients are copied into.
    kernel: Option<sys::Handle>,
    /// The tasks that wait for records newer than they have, or for the
    /// service to stop.
    waiting: RefCell<Vec<Waker>>,
    /// Whether the service stops.
    stopping: Cell<bool>,
    /// The files that records go to and that have not been written out
    /// for the last time.
    pub open_files: Cell<u32>,
}

impl Log {
    /// A log that copies the records of clients into the kernel log
    /// `kernel`, if there is one.
    pub fn new(kernel: Option<sys::Handle>) -> Log {
        Log {
            store: RefCell::new(Store::new(MEMORY)),
            kernel,
            waiting: RefCell::new(Vec::new()),
            stopping: Cell::new(false),
            open_files: Cell::new(0),
        }
    }

    fn wake(&self) {
        for waker in self.waiting.take() {
            waker.wake();
        }
    }

    /// Makes the tasks that wait for records go on, as the service stops.
    pub fn stop(&self) {
        self.stopping.set(true);
        self.wake();
    }

    pub fn stopping(&self) -> bool {
        self.stopping.get()
    }

    /// The lines of the records after sequence number `after`, as many as
    /// fit in `max` bytes, but at least one if there is one, and the
    /// sequence number of the last of them, or None when there are none.
    pub fn lines_after(&self, after: u64, max: usize) -> Option<(Vec<u8>, u64)> {
        let store = self.store.borrow();
        let mut lines = Vec::new();
        let mut last = None;
        for entry in store.after(after, usize::MAX) {
            let line = crate::store::line(entry);
            if last.is_some() && lines.len() + line.len() > max {
                break;
            }
            lines.extend_from_slice(line.as_bytes());
            last = Some(entry.sequence);
        }
        last.map(|last| (lines, last))
    }

    /// Adds a record that a client made, and copies its message into the
    /// kernel log, cut to the most the kernel takes at a character
    /// boundary.
    pub fn add_copied(&self, pid: u32, app_id: String, severity: u8, message: String) {
        if let Some(kernel) = self.kernel {
            let mut len = message.len().min(KERNEL_MESSAGE_MAX);
            while !message.is_char_boundary(len) {
                len -= 1;
            }
            // SAFETY: the pointer and length describe the message's bytes.
            // A failed copy cannot be reported anywhere better.
            unsafe {
                sys::log_write(
                    kernel,
                    u32::from(severity.min(3)),
                    message.as_ptr(),
                    len as u64,
                )
            };
        }
        self.add(now(), pid, app_id, severity, message);
    }

    /// Adds a record made at the monotonic time `timestamp`.
    pub fn add(&self, timestamp: i64, pid: u32, app_id: String, severity: u8, message: String) {
        self.store
            .borrow_mut()
            .push(timestamp, UTC_UNKNOWN, pid, app_id, severity, message);
        self.wake();
    }

    /// The sequence number of the latest record, 0 before the first.
    pub fn latest(&self) -> u64 {
        self.store.borrow().latest()
    }

    /// At most `max` of the records kept after sequence number `after`.
    pub fn after(&self, after: u64, max: usize) -> Vec<Record> {
        self.store.borrow().after(after, max).map(record).collect()
    }

    /// Completes once there is a record after sequence number `after`,
    /// or the service stops.
    pub async fn newer(&self, after: u64) {
        poll_fn(|cx| {
            if self.latest() > after || self.stopping() {
                Poll::Ready(())
            } else {
                self.waiting.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}
