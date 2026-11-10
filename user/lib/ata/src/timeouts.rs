//! The command timeouts of a port (MIKSPEC-0048.R12): when each of its
//! outstanding commands was last issued, how many times each has taken
//! too long, and so what the port must do about them.

/// How long a command may take before the port is reset and its commands
/// are issued again, in nanoseconds, and how many times the same command
/// may take that long before the port's requests fail instead.
pub const LIMIT: i64 = 30_000_000_000;
pub const TIMES: u32 = 3;

/// What a port must do about its outstanding commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing yet.
    Wait,
    /// Reset the port, set it up again, and issue them again.
    Reissue,
    /// Fail them all, and reset the port and set it up again.
    Fail,
}

/// For each of a port's 32 command slots with an outstanding command,
/// when it was last issued and how many times it has taken `LIMIT`.
#[derive(Clone, Debug, Default)]
pub struct Timeouts {
    slots: [Option<(i64, u32)>; 32],
}

impl Timeouts {
    /// Notes that the command in `slot` was issued at `now`.
    pub fn issued(&mut self, slot: u32, now: i64) {
        self.slots[slot as usize] = Some((now, 0));
    }

    /// Notes that the command in `slot` completed or failed.
    pub fn completed(&mut self, slot: u32) {
        self.slots[slot as usize] = None;
    }

    /// What the port must do at `now`. When a command has been outstanding
    /// for `LIMIT`, every outstanding command is issued again from `now`,
    /// unless one has now taken `LIMIT` `TIMES` times: then they all fail
    /// and are forgotten.
    pub fn check(&mut self, now: i64) -> Action {
        let late = |at: i64| now.saturating_sub(at) >= LIMIT;
        if !self.slots.iter().flatten().any(|&(at, _)| late(at)) {
            return Action::Wait;
        }
        let mut fail = false;
        for (at, times) in self.slots.iter_mut().flatten() {
            if late(*at) {
                *times += 1;
                fail |= *times >= TIMES;
            }
            *at = now;
        }
        if fail {
            self.slots = [None; 32];
            return Action::Fail;
        }
        Action::Reissue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1_000_000_000;

    #[test]
    fn waits_for_commands_that_complete_in_time() {
        let mut t = Timeouts::default();
        assert_eq!(t.check(100 * S), Action::Wait);
        t.issued(3, 0);
        assert_eq!(t.check(29 * S), Action::Wait);
        t.completed(3);
        assert_eq!(t.check(100 * S), Action::Wait);
    }

    #[test]
    fn reissues_twice_and_then_fails_the_same_command() {
        let mut t = Timeouts::default();
        t.issued(0, 0);
        t.issued(5, 20 * S);
        // Slot 0 is late: both are issued again, from 30 s.
        assert_eq!(t.check(30 * S), Action::Reissue);
        assert_eq!(t.check(59 * S), Action::Wait);
        assert_eq!(t.check(60 * S), Action::Reissue);
        // The third time slot 0 takes too long, they all fail.
        assert_eq!(t.check(90 * S), Action::Fail);
        assert_eq!(t.check(200 * S), Action::Wait);
    }

    #[test]
    fn counts_each_command_on_its_own() {
        let mut t = Timeouts::default();
        t.issued(1, 0);
        assert_eq!(t.check(30 * S), Action::Reissue);
        assert_eq!(t.check(60 * S), Action::Reissue);
        // A new command in that slot starts again from none.
        t.completed(1);
        t.issued(1, 60 * S);
        assert_eq!(t.check(90 * S), Action::Reissue);
        t.issued(2, 90 * S);
        assert_eq!(t.check(120 * S), Action::Reissue);
        assert_eq!(t.check(150 * S), Action::Fail);
    }
}
