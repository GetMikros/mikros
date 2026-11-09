//! The records of the log service, kept in memory in the order they
//! came, each with its sequence number, until they take more than the
//! store's limit, when the oldest go; and the text lines of records in
//! the log's files.

#![no_std]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::String;
use core::fmt::Write;

#[cfg(test)]
mod tests;

/// What a record takes besides its app ID and message, for the limit.
const FIXED: usize = 48;

/// A record of the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The record's number, one more than the record's before it, from 1.
    pub sequence: u64,
    /// The monotonic time it was made at, in nanoseconds.
    pub timestamp: i64,
    /// The UTC time it was made at, in nanoseconds since the epoch.
    pub utc: i64,
    pub pid: u32,
    pub app_id: String,
    /// 0 for debug, 1 for info, 2 for warning, 3 for error.
    pub severity: u8,
    pub message: String,
}

impl Entry {
    /// The bytes that the record counts for against the limit.
    fn size(&self) -> usize {
        FIXED + self.app_id.len() + self.message.len()
    }
}

/// The records kept, oldest first.
pub struct Store {
    records: VecDeque<Entry>,
    /// What the records count for together.
    bytes: usize,
    limit: usize,
    /// The sequence number of the latest record, 0 before the first.
    latest: u64,
}

impl Store {
    /// A store that keeps records while they take at most `limit` bytes,
    /// and always the latest one.
    pub const fn new(limit: usize) -> Store {
        Store {
            records: VecDeque::new(),
            bytes: 0,
            limit,
            latest: 0,
        }
    }

    /// Adds a record, which gets the next sequence number, and drops the
    /// oldest records while the records take more than the limit.
    /// Returns the record's sequence number.
    pub fn push(
        &mut self,
        timestamp: i64,
        utc: i64,
        pid: u32,
        app_id: String,
        severity: u8,
        message: String,
    ) -> u64 {
        self.latest += 1;
        let entry = Entry {
            sequence: self.latest,
            timestamp,
            utc,
            pid,
            app_id,
            severity,
            message,
        };
        self.bytes += entry.size();
        self.records.push_back(entry);
        while self.bytes > self.limit && self.records.len() > 1 {
            let oldest = self.records.pop_front().expect("more than one record");
            self.bytes -= oldest.size();
        }
        self.latest
    }

    /// The sequence number of the latest record, 0 before the first.
    pub fn latest(&self) -> u64 {
        self.latest
    }

    /// The bytes that the records count for together.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// At most `max` of the records kept whose sequence numbers are
    /// greater than `after`, oldest first.
    pub fn after(&self, after: u64, max: usize) -> impl Iterator<Item = &Entry> {
        // Sequence numbers rise by one from the oldest record kept.
        let first = self.records.front().map_or(0, |e| e.sequence);
        let skip = after.saturating_add(1).saturating_sub(first);
        let skip = usize::try_from(skip).unwrap_or(usize::MAX);
        self.records.iter().skip(skip).take(max)
    }
}

/// The record as a line of the log's files, with its line feed: its UTC
/// time in RFC 3339 format or `-` while it was not known, its monotonic
/// time in seconds with nine decimals, its severity as a letter, its
/// pid, its app ID and its message, separated by single spaces. The
/// message's backslashes are written as `\\` and its line feeds as `\n`.
pub fn line(entry: &Entry) -> String {
    let mut line = String::new();
    match entry.utc {
        0 => line.push('-'),
        utc => push_utc(&mut line, utc),
    }
    let mono = entry.timestamp.max(0);
    let severity = match entry.severity {
        0 => 'D',
        1 => 'I',
        2 => 'W',
        _ => 'E',
    };
    let _ = write!(
        line,
        " {}.{:09} {severity} {} {} ",
        mono / NANOS,
        mono % NANOS,
        entry.pid,
        entry.app_id
    );
    for c in entry.message.chars() {
        match c {
            '\\' => line.push_str("\\\\"),
            '\n' => line.push_str("\\n"),
            c => line.push(c),
        }
    }
    line.push('\n');
    line
}

const NANOS: i64 = 1_000_000_000;

/// Appends `utc`, nanoseconds since 1970-01-01T00:00:00Z, in RFC 3339
/// format with nine decimals.
fn push_utc(out: &mut String, utc: i64) {
    let seconds = utc.div_euclid(NANOS);
    let nanos = utc.rem_euclid(NANOS);
    let (days, time) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil(days);
    let _ = write!(
        out,
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{nanos:09}Z",
        time / 3600,
        time / 60 % 60,
        time % 60
    );
}

/// The year, month and day of `days` since 1970-01-01, in the proleptic
/// Gregorian calendar.
fn civil(days: i64) -> (i64, u32, u32) {
    // Counted from 0000-03-01, in eras of 400 years, which repeat.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
