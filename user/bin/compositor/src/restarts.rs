//! How often the compositor starts a part again after its component
//! exited (MIKSPEC-0075.R21).

use std::collections::VecDeque;

/// The most starts again that a part gets within `WINDOW`.
pub const LIMIT: usize = 5;
/// The time that the limit counts over, in nanoseconds.
pub const WINDOW: i64 = 60_000_000_000;

/// The starts again of one part, by the time of the monotonic clock.
#[derive(Default)]
pub struct Restarts {
    at: VecDeque<i64>,
}

impl Restarts {
    /// Whether the part may be started again at `now`, which counts the
    /// start when it may.
    pub fn allow(&mut self, now: i64) -> bool {
        while self.at.front().is_some_and(|&t| now - t >= WINDOW) {
            self.at.pop_front();
        }
        if self.at.len() >= LIMIT {
            return false;
        }
        self.at.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: i64 = 1_000_000_000;

    #[test]
    fn allows_five_starts_within_a_minute() {
        let mut restarts = Restarts::default();
        for i in 0..LIMIT as i64 {
            assert!(restarts.allow(i * SECOND));
        }
        assert!(!restarts.allow(10 * SECOND));
        assert!(!restarts.allow(59 * SECOND));
    }

    #[test]
    fn allows_a_start_when_the_oldest_left_the_window() {
        let mut restarts = Restarts::default();
        for i in 0..LIMIT as i64 {
            assert!(restarts.allow(i * SECOND));
        }
        assert!(!restarts.allow(59 * SECOND));
        assert!(restarts.allow(60 * SECOND));
        assert!(!restarts.allow(60 * SECOND));
    }

    #[test]
    fn refused_starts_do_not_count() {
        let mut restarts = Restarts::default();
        for _ in 0..LIMIT {
            assert!(restarts.allow(0));
        }
        for i in 1..50 {
            assert!(!restarts.allow(i * SECOND));
        }
        assert!(restarts.allow(60 * SECOND));
    }

    #[test]
    fn slow_exits_never_hit_the_limit() {
        let mut restarts = Restarts::default();
        for i in 0..100 {
            assert!(restarts.allow(i * 20 * SECOND));
        }
    }
}
