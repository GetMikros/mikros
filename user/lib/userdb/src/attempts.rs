//! Failed attempts in a row for each name, and the delay they bring
//! on the next attempt.

use alloc::collections::BTreeMap;
use alloc::string::String;

/// Failures in a row after which attempts are delayed.
const FREE: u32 = 5;
/// The longest delay, in seconds.
const DELAY_MAX: u64 = 300;

#[derive(Default)]
pub struct Attempts {
    failures: BTreeMap<String, u32>,
}

impl Attempts {
    pub fn new() -> Attempts {
        Attempts::default()
    }

    /// The seconds to wait before checking the next attempt for `name`:
    /// none until `FREE` attempts in a row failed, then 1 second,
    /// doubling with each further failure up to `DELAY_MAX`.
    pub fn delay(&self, name: &str) -> u64 {
        let failures = self.failures.get(name).copied().unwrap_or(0);
        match failures.checked_sub(FREE) {
            None => 0,
            Some(over) => 1u64.checked_shl(over).unwrap_or(u64::MAX).min(DELAY_MAX),
        }
    }

    pub fn failed(&mut self, name: &str) {
        let failures = self.failures.entry(String::from(name)).or_default();
        *failures = failures.saturating_add(1);
    }

    pub fn succeeded(&mut self, name: &str) {
        self.failures.remove(name);
    }
}
