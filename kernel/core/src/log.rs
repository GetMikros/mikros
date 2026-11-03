//! The kernel log: a ring buffer of fixed-size records that writers never
//! wait on, and the text lines records are written out as.

use crate::sync::SpinLock;
use alloc::vec::Vec;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};

/// The longest message, in bytes.
pub const MESSAGE_MAX: usize = 256;
/// The size of a record.
pub const RECORD_SIZE: usize = 288;
const WORDS: usize = RECORD_SIZE / 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Severity {
    Debug = 0,
    Info = 1,
    Warning = 2,
    Error = 3,
}

impl Severity {
    /// The severity with value `value`, if there is one.
    pub fn from_u32(value: u32) -> Option<Severity> {
        Some(match value {
            0 => Severity::Debug,
            1 => Severity::Info,
            2 => Severity::Warning,
            3 => Severity::Error,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Source {
    Kernel = 0,
    User = 1,
}

/// A log record, laid out as it is stored and copied out.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub sequence: u64,
    pub timestamp: i64,
    pub process_koid: u64,
    pub severity: u8,
    pub source: u8,
    pub message_len: u16,
    pub reserved: u32,
    pub message: [u8; MESSAGE_MAX],
}

const _: () = assert!(size_of::<Record>() == RECORD_SIZE);

impl Record {
    pub fn message(&self) -> &[u8] {
        &self.message[..usize::from(self.message_len).min(MESSAGE_MAX)]
    }

    /// The record as it is copied out.
    pub fn bytes(&self) -> [u8; RECORD_SIZE] {
        let mut out = [0; RECORD_SIZE];
        for (bytes, word) in out.as_chunks_mut::<8>().0.iter_mut().zip(self.to_words()) {
            *bytes = word.to_le_bytes();
        }
        out
    }

    fn to_words(self) -> [u64; WORDS] {
        let mut words = [0; WORDS];
        words[0] = self.sequence;
        words[1] = self.timestamp as u64;
        words[2] = self.process_koid;
        words[3] = u64::from(self.severity)
            | u64::from(self.source) << 8
            | u64::from(self.message_len) << 16
            | u64::from(self.reserved) << 32;
        for (word, bytes) in words[4..].iter_mut().zip(self.message.as_chunks::<8>().0) {
            *word = u64::from_le_bytes(*bytes);
        }
        words
    }

    fn from_words(words: &[u64; WORDS]) -> Record {
        let mut message = [0; MESSAGE_MAX];
        for (bytes, word) in message.as_chunks_mut::<8>().0.iter_mut().zip(&words[4..]) {
            bytes.copy_from_slice(&word.to_le_bytes());
        }
        Record {
            sequence: words[0],
            timestamp: words[1] as i64,
            process_koid: words[2],
            severity: words[3] as u8,
            source: (words[3] >> 8) as u8,
            message_len: (words[3] >> 16) as u16,
            reserved: (words[3] >> 32) as u32,
            message,
        }
    }
}

/// One record in the ring. `state` is twice the sequence of the record it
/// holds, plus one while that record is being written.
struct Slot {
    state: AtomicU64,
    /// The newest sequence whose writer found the slot busy and dropped
    /// its record.
    dropped: AtomicU64,
    words: [AtomicU64; WORDS],
}

impl Slot {
    const fn new() -> Slot {
        Slot {
            state: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            words: [const { AtomicU64::new(0) }; WORDS],
        }
    }
}

/// The result of looking up a record by sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "records are copied out by value, without an allocator"
)]
pub enum Lookup {
    Found(Record),
    /// The record is still being written, or not yet.
    Pending,
    /// A newer record replaced it, or a writer had to drop it.
    Gone,
}

/// A ring of `SLOTS` records.
pub struct Log<const SLOTS: usize> {
    slots: [Slot; SLOTS],
    /// The sequence of the newest record started.
    head: AtomicU64,
    /// The sequence of the newest record written out by `drain`.
    drained: AtomicU64,
    draining: AtomicBool,
}

impl<const SLOTS: usize> Default for Log<SLOTS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SLOTS: usize> Log<SLOTS> {
    pub const fn new() -> Self {
        Log {
            slots: [const { Slot::new() }; SLOTS],
            head: AtomicU64::new(0),
            drained: AtomicU64::new(0),
            draining: AtomicBool::new(false),
        }
    }

    fn slot(&self, sequence: u64) -> &Slot {
        &self.slots[((sequence - 1) % SLOTS as u64) as usize]
    }

    /// Appends a record and returns its sequence. Never waits: when a
    /// writer that started `SLOTS` records earlier still holds the slot,
    /// the new record is dropped and its sequence is skipped.
    pub fn write(
        &self,
        timestamp: i64,
        process_koid: u64,
        severity: Severity,
        source: Source,
        message: &[u8],
    ) -> u64 {
        let sequence = self.head.fetch_add(1, Ordering::Relaxed) + 1;
        let slot = self.slot(sequence);
        let state = slot.state.load(Ordering::Relaxed);
        let claimed = state % 2 == 0
            && state < sequence * 2
            && slot
                .state
                .compare_exchange(
                    state,
                    sequence * 2 + 1,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok();
        if !claimed {
            slot.dropped.fetch_max(sequence, Ordering::Release);
            return sequence;
        }
        let len = message.len().min(MESSAGE_MAX);
        let mut record = Record {
            sequence,
            timestamp,
            process_koid,
            severity: severity as u8,
            source: source as u8,
            message_len: len as u16,
            reserved: 0,
            message: [0; MESSAGE_MAX],
        };
        record.message[..len].copy_from_slice(&message[..len]);
        for (word, value) in slot.words.iter().zip(record.to_words()) {
            word.store(value, Ordering::Relaxed);
        }
        slot.state.store(sequence * 2, Ordering::Release);
        sequence
    }

    /// The sequence of the newest record started, 0 before the first.
    pub fn head(&self) -> u64 {
        self.head.load(Ordering::Acquire)
    }

    /// Looks up the record with `sequence`.
    pub fn get(&self, sequence: u64) -> Lookup {
        if sequence == 0 || sequence > self.head() {
            return Lookup::Pending;
        }
        let slot = self.slot(sequence);
        let before = slot.state.load(Ordering::Acquire);
        if before > sequence * 2 + 1 || slot.dropped.load(Ordering::Acquire) >= sequence {
            return Lookup::Gone;
        }
        if before != sequence * 2 {
            return Lookup::Pending;
        }
        let mut words = [0; WORDS];
        for (value, word) in words.iter_mut().zip(&slot.words) {
            *value = word.load(Ordering::Relaxed);
        }
        fence(Ordering::Acquire);
        if slot.state.load(Ordering::Relaxed) != before {
            return Lookup::Gone;
        }
        Lookup::Found(Record::from_words(&words))
    }

    /// The record with the smallest sequence above `after` that is still
    /// in the ring, if one is written.
    pub fn next_after(&self, after: u64) -> Option<Record> {
        let head = self.head();
        let oldest = head.saturating_sub(SLOTS as u64) + 1;
        // No record follows the largest sequence number.
        let first = after.checked_add(1)?;
        for sequence in first.max(oldest)..=head {
            match self.get(sequence) {
                Lookup::Found(record) => return Some(record),
                Lookup::Gone => continue,
                Lookup::Pending => return None,
            }
        }
        None
    }

    /// Passes each record not yet drained, in order, to `out`. Only one
    /// caller drains at a time; a caller that finds another one draining
    /// returns at once, and the other one also drains its records. A record
    /// still being written stops the drain until its writer drains.
    pub fn drain(&self, mut out: impl FnMut(&Record)) {
        loop {
            if self.draining.swap(true, Ordering::Acquire) {
                return;
            }
            let mut drained = self.drained.load(Ordering::Relaxed);
            let mut stopped = false;
            while drained < self.head() {
                match self.get(drained + 1) {
                    Lookup::Found(record) => out(&record),
                    Lookup::Gone => {}
                    Lookup::Pending => {
                        stopped = true;
                        break;
                    }
                }
                drained += 1;
            }
            self.drained.store(drained, Ordering::Relaxed);
            self.draining.store(false, Ordering::Release);
            // Records written while the flag was held, by callers that
            // returned at once, are drained by another round.
            if stopped || drained >= self.head() {
                return;
            }
        }
    }
}

/// The readers of a log, which writers notify without waiting.
///
/// A notification runs only where the caller may take the locks that
/// notifying a reader takes, and only while the list is free. Otherwise
/// it stays pending: the user of the list runs it once done with the
/// list, and `run_pending` runs it where locks may be taken.
pub struct Readers<T> {
    list: SpinLock<Vec<T>>,
    pending: AtomicBool,
}

impl<T> Default for Readers<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Readers<T> {
    pub const fn new() -> Self {
        Readers {
            list: SpinLock::new(Vec::new()),
            pending: AtomicBool::new(false),
        }
    }

    /// Calls `each` with every reader: now when `may_lock` holds and the
    /// list is free, and otherwise later, as described above.
    pub fn notify(&self, may_lock: bool, each: impl Fn(&T)) {
        self.pending.store(true, Ordering::Release);
        if may_lock {
            self.run_pending(each);
        }
    }

    /// Runs a pending notification with `each`, unless the list is in
    /// use. Must be called only where the locks `each` takes may be
    /// taken.
    pub fn run_pending(&self, each: impl Fn(&T)) {
        // A notification that comes while this one runs, and finds the
        // list in use, is run by another round.
        while self.pending.load(Ordering::Acquire) {
            let Some(list) = self.list.try_lock() else {
                return;
            };
            self.pending.store(false, Ordering::Release);
            for reader in list.iter() {
                each(reader);
            }
        }
    }

    /// Calls `f` with the list, then runs with `each` a notification that
    /// came meanwhile. Must be called only where the locks `each` takes
    /// may be taken.
    pub fn with<R>(&self, f: impl FnOnce(&mut Vec<T>) -> R, each: impl Fn(&T)) -> R {
        let result = f(&mut self.list.lock());
        self.run_pending(each);
        result
    }
}

/// A message being formatted, cut at `MESSAGE_MAX` bytes on a character
/// boundary.
pub struct Message {
    bytes: [u8; MESSAGE_MAX],
    len: usize,
}

impl Default for Message {
    fn default() -> Self {
        Message {
            bytes: [0; MESSAGE_MAX],
            len: 0,
        }
    }
}

impl Message {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl Write for Message {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            let len = c.len_utf8();
            if self.len + len > MESSAGE_MAX {
                break;
            }
            c.encode_utf8(&mut self.bytes[self.len..]);
            self.len += len;
        }
        Ok(())
    }
}

/// The longest line `line` writes, line break excluded.
pub const LINE_MAX: usize = 320;

/// A record's text line, built in place.
pub struct Line {
    bytes: [u8; LINE_MAX + 2],
    len: usize,
}

impl Line {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn push(&mut self, bytes: &[u8]) {
        let end = (self.len + bytes.len()).min(self.bytes.len());
        self.bytes[self.len..end].copy_from_slice(&bytes[..end - self.len]);
        self.len = end;
    }
}

impl Write for Line {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push(s.as_bytes());
        Ok(())
    }
}

/// The text line of `record`, ending with `newline`, which is at most two
/// bytes. The timestamp is in nanoseconds.
pub fn line(record: &Record, newline: &[u8]) -> Line {
    let mut line = Line {
        bytes: [0; LINE_MAX + 2],
        len: 0,
    };
    let ns = record.timestamp.max(0) as u64;
    let level = match record.severity {
        0 => 'D',
        1 => 'I',
        2 => 'W',
        _ => 'E',
    };
    let _ = write!(
        line,
        "[{}.{:06}] {level} ",
        ns / 1_000_000_000,
        ns % 1_000_000_000 / 1000
    );
    let _ = if record.source == Source::Kernel as u8 {
        write!(line, "kernel: ")
    } else {
        write!(line, "p{}: ", record.process_koid)
    };
    for &b in record.message() {
        line.push(&[if b < 0x20 || b == 0x7f { b'?' } else { b }]);
    }
    line.len = line.len.min(LINE_MAX);
    line.push(&newline[..newline.len().min(2)]);
    line
}

#[cfg(test)]
mod tests;
