use super::*;
use alloc::string::ToString;
use alloc::vec::Vec;

fn push(store: &mut Store, message: &str) -> u64 {
    store.push(10, 0, 7, "system.test".to_string(), 1, message.to_string())
}

fn messages<'a>(entries: impl Iterator<Item = &'a Entry>) -> Vec<&'a str> {
    entries.map(|e| e.message.as_str()).collect()
}

#[test]
fn numbers_records_from_one() {
    let mut store = Store::new(1 << 20);
    assert_eq!(store.latest(), 0);
    assert_eq!(push(&mut store, "a"), 1);
    assert_eq!(push(&mut store, "b"), 2);
    assert_eq!(store.latest(), 2);
    let first = store.after(0, 1).next().unwrap();
    assert_eq!(
        *first,
        Entry {
            sequence: 1,
            timestamp: 10,
            utc: 0,
            pid: 7,
            app_id: "system.test".to_string(),
            severity: 1,
            message: "a".to_string(),
        }
    );
}

#[test]
fn reads_after_a_sequence_number() {
    let mut store = Store::new(1 << 20);
    for m in ["a", "b", "c", "d"] {
        push(&mut store, m);
    }
    assert_eq!(messages(store.after(0, 32)), ["a", "b", "c", "d"]);
    assert_eq!(messages(store.after(2, 32)), ["c", "d"]);
    assert_eq!(messages(store.after(1, 2)), ["b", "c"]);
    assert!(store.after(4, 32).next().is_none());
    assert!(store.after(u64::MAX, 32).next().is_none());
}

#[test]
fn drops_the_oldest_records_over_the_limit() {
    // Each record counts for FIXED, its app ID and its message.
    let size = FIXED + "system.test".len() + 1;
    let mut store = Store::new(3 * size);
    for m in ["a", "b", "c", "d", "e"] {
        push(&mut store, m);
    }
    assert_eq!(store.bytes(), 3 * size);
    assert_eq!(messages(store.after(0, 32)), ["c", "d", "e"]);
    // A reader that fell behind gets what is left.
    assert_eq!(messages(store.after(1, 32)), ["c", "d", "e"]);
    assert_eq!(messages(store.after(3, 32)), ["d", "e"]);
    assert_eq!(store.latest(), 5);
}

#[test]
fn keeps_the_latest_record_even_over_the_limit() {
    let mut store = Store::new(1);
    push(&mut store, "a");
    push(&mut store, "bigger than the limit");
    assert_eq!(messages(store.after(0, 32)), ["bigger than the limit"]);
}

fn entry(utc: i64, timestamp: i64, severity: u8, message: &str) -> Entry {
    Entry {
        sequence: 1,
        timestamp,
        utc,
        pid: 42,
        app_id: "system.driver.nvme".to_string(),
        severity,
        message: message.to_string(),
    }
}

#[test]
fn writes_a_record_as_a_line() {
    let e = entry(
        1_794_186_189_000_000_123,
        12_345_678_901,
        1,
        "namespace 2 skipped",
    );
    assert_eq!(
        line(&e),
        "2026-11-09T01:03:09.000000123Z 12.345678901 I 42 system.driver.nvme namespace 2 skipped\n"
    );
}

#[test]
fn writes_a_dash_while_utc_is_not_known() {
    let e = entry(0, 5, 3, "failed");
    assert_eq!(line(&e), "- 0.000000005 E 42 system.driver.nvme failed\n");
}

#[test]
fn writes_each_severity_as_a_letter() {
    let letters: Vec<char> = (0..4)
        .map(|s| line(&entry(0, 0, s, "")).chars().nth(14).unwrap())
        .collect();
    assert_eq!(letters, ['D', 'I', 'W', 'E']);
}

#[test]
fn escapes_backslashes_and_line_feeds() {
    let e = entry(0, 0, 1, "a\\b\nc\\n");
    assert_eq!(
        line(&e),
        "- 0.000000000 I 42 system.driver.nvme a\\\\b\\nc\\\\n\n"
    );
}

#[test]
fn writes_utc_dates_across_the_calendar() {
    let utc = |seconds: i64| {
        let l = line(&entry(seconds * 1_000_000_000, 0, 1, ""));
        l[..l.find(' ').unwrap()].to_string()
    };
    assert_eq!(utc(951_868_799), "2000-02-29T23:59:59.000000000Z");
    assert_eq!(utc(1_735_646_400), "2024-12-31T12:00:00.000000000Z");
    assert_eq!(utc(4_107_542_400), "2100-03-01T00:00:00.000000000Z");
    assert_eq!(utc(1), "1970-01-01T00:00:01.000000000Z");
}
