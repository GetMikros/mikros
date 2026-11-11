//! The system log as lines on a screen: each record as one line of its
//! time since boot, its app ID and its message, cut to the screen's
//! width, and the most recent lines, as many as the screen shows.

#![no_std]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::String;
use core::fmt::Write;

#[cfg(test)]
mod tests;

const NANOS: i64 = 1_000_000_000;

/// The line of a record made at `timestamp`, monotonic nanoseconds, by
/// `app_id`, with `message`: the time in seconds with six decimals in
/// brackets, a space, the app ID, a colon, a space and the message. Each
/// control character is a `?`, so that the line stays one line and holds
/// no escape sequence.
pub fn line(timestamp: i64, app_id: &str, message: &str) -> String {
    let time = timestamp.max(0);
    let mut line = String::new();
    let _ = write!(line, "[{}.{:06}] ", time / NANOS, time % NANOS / 1000);
    for c in app_id.chars().chain(": ".chars()).chain(message.chars()) {
        line.push(if c.is_control() { '?' } else { c });
    }
    line
}

/// The longest start of `line` that takes at most `cols` columns, when
/// `columns` gives the columns of each character.
pub fn fit(line: &str, cols: usize, columns: impl Fn(char) -> usize) -> &str {
    let mut used = 0;
    for (at, c) in line.char_indices() {
        used += columns(c);
        if used > cols {
            return &line[..at];
        }
    }
    line
}

/// The lines of the most recent records, at most `max`, oldest first,
/// each with its record's sequence number.
pub struct Tail {
    lines: VecDeque<(u64, String)>,
    max: usize,
    /// The sequence number of the latest record taken, 0 before the first.
    latest: u64,
}

impl Tail {
    pub fn new(max: usize) -> Tail {
        Tail {
            lines: VecDeque::new(),
            max,
            latest: 0,
        }
    }

    /// Takes `line`, the line of record `sequence`, and drops the oldest
    /// line when there are more than `max`. A record no later than the
    /// latest taken is left out, so that records read twice show once.
    /// Returns whether the line was taken.
    pub fn push(&mut self, sequence: u64, line: String) -> bool {
        if sequence <= self.latest {
            return false;
        }
        self.latest = sequence;
        self.lines.push_back((sequence, line));
        while self.lines.len() > self.max {
            self.lines.pop_front();
        }
        true
    }

    pub fn latest(&self) -> u64 {
        self.latest
    }

    /// The most recent lines, at most `n`, oldest first.
    pub fn last(&self, n: usize) -> impl Iterator<Item = (u64, &str)> {
        let skip = self.lines.len().saturating_sub(n);
        self.lines.iter().skip(skip).map(|(s, l)| (*s, l.as_str()))
    }
}
