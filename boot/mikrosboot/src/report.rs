//! Reporting a failure while boot services run.

use alloc::string::{String, ToString};
use core::fmt::{self, Display};
use core::time::Duration;

/// A step that failed while boot services run.
#[derive(Debug)]
pub struct Failure {
    step: &'static str,
    reason: String,
}

impl Failure {
    pub fn new(step: &'static str, reason: impl Display) -> Failure {
        Failure {
            step,
            reason: reason.to_string(),
        }
    }
}

impl Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mikrosboot: {} failed: {}", self.step, self.reason)
    }
}

/// How long a failure stays on the screen without a key press.
pub const FAILURE_WAIT: Duration = Duration::from_secs(30);
/// How often the keyboard is checked meanwhile.
pub const POLL: Duration = Duration::from_millis(100);

/// The console a failure is reported on.
pub trait Console {
    /// Writes `line` and ends the line.
    fn write_line(&mut self, line: fmt::Arguments);

    /// Drops the keys pressed so far.
    fn reset_input(&mut self);

    /// Whether a key was pressed, taking it.
    fn key_pressed(&mut self) -> bool;

    /// Waits for `time`.
    fn stall(&mut self, time: Duration);
}

/// Shows `failure` on `console` and waits for a key press or
/// `FAILURE_WAIT`, whichever comes first.
pub fn report(console: &mut impl Console, failure: &Failure) {
    console.write_line(format_args!("{failure}"));
    console.write_line(format_args!("Press a key to continue."));
    console.reset_input();
    let mut waited = Duration::ZERO;
    while waited < FAILURE_WAIT {
        if console.key_pressed() {
            break;
        }
        console.stall(POLL);
        waited += POLL;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// A console whose key is pressed after `key_after` checks, if ever.
    #[derive(Default)]
    struct Fake {
        lines: Vec<String>,
        reset: bool,
        checks: u32,
        key_after: Option<u32>,
        waited: Duration,
    }

    impl Console for Fake {
        fn write_line(&mut self, line: fmt::Arguments) {
            self.lines.push(line.to_string());
        }

        fn reset_input(&mut self) {
            assert!(!self.reset, "reset twice");
            self.reset = true;
        }

        fn key_pressed(&mut self) -> bool {
            assert!(self.reset, "keys read before the reset");
            self.checks += 1;
            self.key_after.is_some_and(|n| self.checks > n)
        }

        fn stall(&mut self, time: Duration) {
            self.waited += time;
        }
    }

    #[test]
    fn names_the_failed_step() {
        let mut console = Fake {
            key_after: Some(0),
            ..Fake::default()
        };
        report(&mut console, &Failure::new("reading x", "not found"));
        assert_eq!(
            console.lines,
            [
                "mikrosboot: reading x failed: not found",
                "Press a key to continue."
            ]
        );
    }

    #[test]
    fn stops_waiting_at_a_key_press() {
        let mut console = Fake {
            key_after: Some(0),
            ..Fake::default()
        };
        report(&mut console, &Failure::new("a", "b"));
        assert_eq!((console.checks, console.waited), (1, Duration::ZERO));
        let mut console = Fake {
            key_after: Some(5),
            ..Fake::default()
        };
        report(&mut console, &Failure::new("a", "b"));
        assert_eq!((console.checks, console.waited), (6, POLL * 5));
    }

    #[test]
    fn waits_thirty_seconds_without_a_key() {
        let mut console = Fake::default();
        report(&mut console, &Failure::new("a", "b"));
        assert_eq!(console.waited, Duration::from_secs(30));
        assert_eq!(console.checks, 300);
    }
}
