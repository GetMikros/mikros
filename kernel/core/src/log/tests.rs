use super::*;
use core::mem::offset_of;
use std::string::String;
use std::vec::Vec;

#[test]
fn records_have_the_fixed_layout() {
    assert_eq!(size_of::<Record>(), 288);
    let offsets = [
        offset_of!(Record, sequence),
        offset_of!(Record, timestamp),
        offset_of!(Record, process_koid),
        offset_of!(Record, severity),
        offset_of!(Record, source),
        offset_of!(Record, message_len),
        offset_of!(Record, reserved),
        offset_of!(Record, message),
    ];
    assert_eq!(offsets, [0x00, 0x08, 0x10, 0x18, 0x19, 0x1A, 0x1C, 0x20]);
}

#[test]
fn numbers_records_from_one_and_keeps_their_fields() {
    let log = Log::<4>::new();
    assert_eq!(log.write(5, 0, Severity::Info, Source::Kernel, b"hi"), 1);
    assert_eq!(log.write(6, 9, Severity::Error, Source::User, b"x"), 2);
    let Lookup::Found(r) = log.get(2) else {
        panic!("record 2 missing")
    };
    assert_eq!((r.sequence, r.timestamp, r.process_koid), (2, 6, 9));
    assert_eq!((r.severity, r.source, r.message()), (3, 1, &b"x"[..]));
    assert!(r.message[1..].iter().all(|&b| b == 0));
    assert_eq!(log.get(3), Lookup::Pending);
}

#[test]
fn replaces_the_oldest_record_when_full() {
    let log = Log::<4>::new();
    for i in 0..6u8 {
        log.write(0, 0, Severity::Debug, Source::Kernel, &[b'a' + i]);
    }
    assert_eq!(log.get(2), Lookup::Gone);
    assert_eq!(log.next_after(0).map(|r| r.sequence), Some(3));
    assert_eq!(log.next_after(4).map(|r| r.sequence), Some(5));
    assert_eq!(log.next_after(6), None);
}

#[test]
fn cuts_long_messages_on_a_character_boundary() {
    let mut message = Message::default();
    let _ = write!(message, "{}", "é".repeat(200));
    assert_eq!(message.as_bytes().len(), 256);
    let mut message = Message::default();
    let _ = write!(message, "x{}", "é".repeat(200));
    assert_eq!(message.as_bytes().len(), 255);
}

fn text(record: &Record) -> String {
    String::from_utf8(line(record, b"\r\n").as_bytes().to_vec()).unwrap()
}

#[test]
fn formats_lines() {
    let log = Log::<4>::new();
    log.write(
        12_345_678_901,
        0,
        Severity::Warning,
        Source::Kernel,
        b"low memory",
    );
    log.write(999, 42, Severity::Debug, Source::User, b"tab\there\x7f");
    let r1 = log.next_after(0).unwrap();
    let r2 = log.next_after(1).unwrap();
    assert_eq!(text(&r1), "[12.345678] W kernel: low memory\r\n");
    assert_eq!(text(&r2), "[0.000000] D p42: tab?here?\r\n");
}

#[test]
fn fits_the_longest_line() {
    let log = Log::<1>::new();
    log.write(
        i64::MAX,
        u64::MAX,
        Severity::Info,
        Source::User,
        &[b'm'; 256],
    );
    let r = log.next_after(0).unwrap();
    let t = text(&r);
    assert!(t.ends_with(&("m".repeat(256) + "\r\n")), "{t}");
}

#[test]
fn drains_each_record_once_in_order() {
    let log = Log::<8>::new();
    let mut seen = Vec::new();
    log.write(0, 0, Severity::Info, Source::Kernel, b"a");
    log.write(0, 0, Severity::Info, Source::Kernel, b"b");
    log.drain(|r| seen.push(r.sequence));
    log.write(0, 0, Severity::Info, Source::Kernel, b"c");
    log.drain(|r| seen.push(r.sequence));
    log.drain(|r| seen.push(r.sequence));
    assert_eq!(seen, [1, 2, 3]);
}

#[test]
fn a_drain_inside_a_drain_returns_at_once() {
    let log = Log::<8>::new();
    let mut seen = Vec::new();
    log.write(0, 0, Severity::Info, Source::Kernel, b"a");
    log.drain(|r| {
        seen.push(r.sequence);
        if r.sequence == 1 {
            // As an interrupt handler would while the drain runs.
            log.write(0, 0, Severity::Info, Source::Kernel, b"b");
            log.drain(|_| panic!("nested drain wrote a record"));
        }
    });
    assert_eq!(seen, [1, 2]);
}

#[test]
fn writers_on_many_threads_lose_nothing_that_fits() {
    let log = Log::<1024>::new();
    std::thread::scope(|s| {
        for t in 0..4u64 {
            let log = &log;
            s.spawn(move || {
                for i in 0..200u64 {
                    log.write(i as i64, t, Severity::Info, Source::User, b"m");
                }
            });
        }
    });
    let mut count = 0;
    log.drain(|_| count += 1);
    assert_eq!(count, 800);
}

#[test]
fn a_dropped_record_does_not_stop_the_drain() {
    let log = Log::<1>::new();
    // A writer that started one ring earlier still holds the slot.
    log.slots[0].state.store(1, Ordering::Relaxed);
    log.head.store(0, Ordering::Relaxed);
    let sequence = log.write(0, 0, Severity::Info, Source::Kernel, b"lost");
    assert_eq!(log.get(sequence), Lookup::Gone);
    log.slots[0].state.store(0, Ordering::Relaxed);
    log.write(0, 0, Severity::Info, Source::Kernel, b"kept");
    let mut seen = Vec::new();
    log.drain(|r| seen.push(r.sequence));
    assert_eq!(seen, [2]);
}

#[test]
fn copies_records_out_in_their_layout() {
    let log: Log<4> = Log::new();
    log.write(1_500, 42, Severity::Warning, Source::User, b"hi");
    let Lookup::Found(record) = log.get(1) else {
        panic!("record 1 is missing");
    };
    let bytes = record.bytes();
    assert_eq!(bytes[0x00..0x08], 1u64.to_le_bytes());
    assert_eq!(bytes[0x08..0x10], 1_500i64.to_le_bytes());
    assert_eq!(bytes[0x10..0x18], 42u64.to_le_bytes());
    assert_eq!(bytes[0x18..0x1C], [2, 1, 2, 0]);
    assert_eq!(&bytes[0x20..0x23], b"hi\0");
    assert!(bytes[0x23..].iter().all(|&b| b == 0));
}

#[test]
fn knows_the_severity_values() {
    assert_eq!(Severity::from_u32(0), Some(Severity::Debug));
    assert_eq!(Severity::from_u32(3), Some(Severity::Error));
    assert_eq!(Severity::from_u32(4), None);
}

#[test]
fn finds_nothing_after_the_largest_sequence_number() {
    let log = Log::<4>::new();
    log.write(0, 0, Severity::Info, Source::Kernel, b"a");
    assert!(log.next_after(u64::MAX).is_none());
    assert_eq!(log.next_after(u64::MAX - 1), None);
    assert!(log.next_after(0).is_some());
}

/// A notification that comes while the reader list is in use, as one
/// from a record written while a reader is being added, runs once the
/// list is free, and reaches the reader added meanwhile.
#[test]
fn notifies_readers_once_the_list_is_free() {
    let readers = Readers::<u32>::new();
    let seen = core::cell::RefCell::new(Vec::new());
    let each = |r: &u32| seen.borrow_mut().push(*r);
    readers.with(|list| list.extend([1, 2]), each);
    assert!(seen.borrow().is_empty());
    readers.with(
        |list| {
            list.push(3);
            readers.notify(true, each);
            assert!(seen.borrow().is_empty());
        },
        each,
    );
    assert_eq!(*seen.borrow(), [1, 2, 3]);
    readers.notify(true, each);
    assert_eq!(*seen.borrow(), [1, 2, 3, 1, 2, 3]);
}

/// A notification made where locks may not be taken runs at the next
/// `run_pending`, once.
#[test]
fn leaves_notifications_pending_where_locks_may_not_be_taken() {
    let readers = Readers::<u32>::new();
    let count = core::cell::Cell::new(0);
    let each = |_: &u32| count.set(count.get() + 1);
    readers.with(|list| list.push(7), each);
    readers.notify(false, each);
    readers.notify(false, each);
    assert_eq!(count.get(), 0);
    readers.run_pending(each);
    assert_eq!(count.get(), 1);
    readers.run_pending(each);
    assert_eq!(count.get(), 1);
}
