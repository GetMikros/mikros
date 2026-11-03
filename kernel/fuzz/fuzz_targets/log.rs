//! Runs the kernel log ring as the kernel does, with writes, lookups and
//! drains, including writes and drains made while a drain passes out a
//! record, and compares every record and text line with a model. Also
//! formats messages and compares them with a model.

#![no_main]

use core::fmt::Write;
use kernel_core::log::{
    LINE_MAX, Log, Lookup, MESSAGE_MAX, Message, RECORD_SIZE, Record, Severity, Source, line,
};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::cell::RefCell;

/// Few slots, so that records are replaced often.
const SLOTS: usize = 4;

#[derive(Arbitrary, Debug, Clone)]
struct Entry {
    timestamp: i64,
    process_koid: u64,
    severity: u8,
    user: bool,
    message: Vec<u8>,
}

#[derive(Arbitrary, Debug)]
enum Op {
    Write(Entry),
    Get(u64),
    /// Looks up a sequence near the head.
    GetNear(u8),
    NextAfter(u64),
    NextAfterNear(u8),
    /// Drains, making these writes, and a drain when `nested` is set,
    /// while the first record is passed out.
    Drain {
        writes: Vec<Entry>,
        nested: bool,
    },
    Line {
        sequence: u8,
        newline: Vec<u8>,
    },
    Format(Vec<String>),
}

const SEVERITIES: [Severity; 4] = [
    Severity::Debug,
    Severity::Info,
    Severity::Warning,
    Severity::Error,
];

fn severity(v: u8) -> Severity {
    SEVERITIES[usize::from(v) % 4]
}

/// The record the ring must hold for `entry` written as `sequence`.
fn expected(sequence: u64, e: &Entry) -> Record {
    let len = e.message.len().min(MESSAGE_MAX);
    let mut message = [0; MESSAGE_MAX];
    message[..len].copy_from_slice(&e.message[..len]);
    Record {
        sequence,
        timestamp: e.timestamp,
        process_koid: e.process_koid,
        severity: severity(e.severity) as u8,
        source: if e.user { Source::User } else { Source::Kernel } as u8,
        message_len: len as u16,
        reserved: 0,
        message,
    }
}

/// The layout records are copied out in.
fn layout(r: &Record) -> [u8; RECORD_SIZE] {
    let mut out = [0; RECORD_SIZE];
    out[0..8].copy_from_slice(&r.sequence.to_le_bytes());
    out[8..16].copy_from_slice(&r.timestamp.to_le_bytes());
    out[16..24].copy_from_slice(&r.process_koid.to_le_bytes());
    out[24] = r.severity;
    out[25] = r.source;
    out[26..28].copy_from_slice(&r.message_len.to_le_bytes());
    out[28..32].copy_from_slice(&r.reserved.to_le_bytes());
    out[32..].copy_from_slice(&r.message);
    out
}

/// The text line of `r`.
fn model_line(r: &Record, newline: &[u8]) -> Vec<u8> {
    let ns = r.timestamp.max(0) as u64;
    let level = ["D", "I", "W"].get(usize::from(r.severity)).unwrap_or(&"E");
    let who = if r.source == Source::Kernel as u8 {
        "kernel".to_owned()
    } else {
        format!("p{}", r.process_koid)
    };
    let mut out = format!(
        "[{}.{:06}] {level} {who}: ",
        ns / 1_000_000_000,
        ns % 1_000_000_000 / 1000
    )
    .into_bytes();
    out.extend(r.message().iter().map(|&b| if b < 0x20 || b == 0x7f { b'?' } else { b }));
    out.truncate(LINE_MAX);
    out.extend(&newline[..newline.len().min(2)]);
    out
}

struct Model {
    /// The entry written as each sequence, from 1.
    written: Vec<Entry>,
    /// The newest sequence drained.
    drained: u64,
}

impl Model {
    fn head(&self) -> u64 {
        self.written.len() as u64
    }

    /// What looking up `sequence` must find: no writer is ever busy, so
    /// only replaced records are gone.
    fn get(&self, sequence: u64) -> Lookup {
        if sequence == 0 || sequence > self.head() {
            Lookup::Pending
        } else if sequence + SLOTS as u64 <= self.head() {
            Lookup::Gone
        } else {
            Lookup::Found(expected(sequence, &self.written[sequence as usize - 1]))
        }
    }

    fn write(&mut self, log: &Log<SLOTS>, e: &Entry) {
        let source = if e.user { Source::User } else { Source::Kernel };
        let s = log.write(e.timestamp, e.process_koid, severity(e.severity), source, &e.message);
        self.written.push(e.clone());
        assert_eq!(s, self.head());
    }
}

fn check_found(lookup: Lookup, model: Lookup) {
    assert_eq!(lookup, model);
    if let Lookup::Found(r) = lookup {
        assert_eq!(r.bytes(), layout(&r));
        assert_eq!(r.message().len(), usize::from(r.message_len));
    }
}

fn drain(log: &Log<SLOTS>, model: &RefCell<Model>, writes: &[Entry], nested: bool) {
    let start = model.borrow().drained;
    let mut out: Vec<Record> = Vec::new();
    let mut first = true;
    log.drain(|r| {
        if first {
            first = false;
            for e in writes.iter().take(2 * SLOTS) {
                model.borrow_mut().write(log, e);
            }
            if nested {
                log.drain(|_| panic!("a drain inside a drain passed out a record"));
            }
        }
        out.push(*r);
    });
    let mut m = model.borrow_mut();
    let head = m.head();
    // Each record comes out once, in order, as it was written.
    assert!(out.windows(2).all(|w| w[0].sequence < w[1].sequence));
    for r in &out {
        assert!(r.sequence > start && r.sequence <= head);
        assert_eq!(*r, expected(r.sequence, &m.written[r.sequence as usize - 1]));
    }
    // Records still in the ring were all passed out.
    let kept = head.saturating_sub(SLOTS as u64).max(start) + 1;
    for s in kept..=head {
        assert!(out.iter().any(|r| r.sequence == s), "record {s} not drained");
    }
    // Without writes during the drain, only replaced records are skipped.
    if writes.is_empty() || out.is_empty() {
        assert_eq!(out.len() as u64, head - head.saturating_sub(SLOTS as u64).max(start));
    }
    m.drained = head;
    drop(m);
    // Everything was drained.
    log.drain(|r| panic!("record {} drained twice", r.sequence));
}

/// The first record after `after` is the oldest one still in the ring
/// after it, since no writer is ever busy.
fn next_after(log: &Log<SLOTS>, m: &Model, after: u64) {
    let oldest = m.head().saturating_sub(SLOTS as u64) + 1;
    let want = after.checked_add(1).and_then(|s| match m.get(s.max(oldest)) {
        Lookup::Found(r) => Some(r),
        _ => None,
    });
    assert_eq!(log.next_after(after), want);
}

fn format(parts: &[String]) {
    let mut message = Message::default();
    let mut model = String::new();
    for part in parts.iter().take(64) {
        let _ = message.write_str(part);
        for c in part.chars() {
            if model.len() + c.len_utf8() > MESSAGE_MAX {
                break;
            }
            model.push(c);
        }
    }
    assert_eq!(message.as_bytes(), model.as_bytes());
}

fuzz_target!(|ops: Vec<Op>| {
    let log = Log::<SLOTS>::new();
    let model = RefCell::new(Model {
        written: Vec::new(),
        drained: 0,
    });
    for op in ops.iter().take(512) {
        let head = model.borrow().head();
        let near = |d: u8| (head + 2).saturating_sub(u64::from(d) % (2 * SLOTS as u64 + 4));
        match op {
            Op::Write(e) => model.borrow_mut().write(&log, e),
            Op::Get(s) => check_found(log.get(*s), model.borrow().get(*s)),
            Op::GetNear(d) => check_found(log.get(near(*d)), model.borrow().get(near(*d))),
            Op::NextAfter(a) => next_after(&log, &model.borrow(), *a),
            Op::NextAfterNear(d) => next_after(&log, &model.borrow(), near(*d)),
            Op::Drain { writes, nested } => drain(&log, &model, writes, *nested),
            Op::Line { sequence, newline } => {
                if let Lookup::Found(r) = log.get(near(*sequence)) {
                    let l = line(&r, newline);
                    assert_eq!(l.as_bytes(), model_line(&r, newline));
                    assert!(l.as_bytes().len() <= LINE_MAX + 2);
                }
            }
            Op::Format(parts) => format(parts),
        }
        assert_eq!(log.head(), model.borrow().head());
    }
    for v in 0..8 {
        assert_eq!(Severity::from_u32(v).map(|s| s as u32), (v < 4).then_some(v));
    }
});
