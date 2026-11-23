//! The questions that are open (MIKSPEC-0037.R55) and the calls of `Ask`
//! (MIKSPEC-0094.R5): which request asks the prompter and which waits for
//! the question that is open for the same grant, which are refused for a
//! minute after a question that nobody answered, and which `Ask` calls
//! come too often.

use super::Identity;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

/// How long, in nanoseconds, a process that waited for a question that
/// nobody answered is refused without a new question.
pub const REFUSED_FOR: i64 = 60_000_000_000;
/// The period, in nanoseconds, in which a user and app ID may make
/// `MOST_ASKS` calls of `Ask`, and how many.
const ASK_PERIOD: i64 = 1_000_000_000;
const MOST_ASKS: usize = 10;

/// The grant that a question is for: the user, app ID, portable checksum
/// and permission, and, for a build, the kernel object ID of its process.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Grant {
    user: u32,
    app_id: String,
    checksum: String,
    permission: String,
    koid: u64,
}

impl Grant {
    pub fn of(client: Identity<'_>, permission: &str) -> Grant {
        Grant {
            user: client.user,
            app_id: client.app_id.into(),
            checksum: client.checksum.into(),
            permission: permission.into(),
            koid: if client.build() { client.koid } else { 0 },
        }
    }
}

/// What a request does about the question for its grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Begin {
    /// No question is open: it asks the prompter, and calls `end`.
    Ask,
    /// A question is open: it waits for its answer.
    Join,
    /// Its process waited for a question that nobody answered a short
    /// time ago: it is `DENIED` without a question.
    Refused,
}

#[derive(Debug, Default)]
pub struct Asks {
    /// The open questions, with the kernel object IDs of the processes
    /// whose requests wait for each, the one that asked first included.
    open: BTreeMap<Grant, Vec<u64>>,
    /// The processes refused for a permission until a time.
    refused: Vec<(u64, String, i64)>,
    /// When each user and app ID made its calls of `Ask`.
    calls: BTreeMap<(u32, String), VecDeque<i64>>,
}

impl Asks {
    /// A request of the process `koid` for `grant` at time `now`.
    pub fn begin(&mut self, grant: &Grant, koid: u64, now: i64) -> Begin {
        self.refused.retain(|(_, _, until)| *until > now);
        if koid != 0
            && self
                .refused
                .iter()
                .any(|(k, p, _)| *k == koid && *p == grant.permission)
        {
            return Begin::Refused;
        }
        match self.open.get_mut(grant) {
            Some(waiting) => {
                waiting.push(koid);
                Begin::Join
            }
            None => {
                self.open.insert(grant.clone(), alloc::vec![koid]);
                Begin::Ask
            }
        }
    }

    /// The question for `grant` ended at `now`: when nobody answered
    /// it, the processes that waited for it are refused for a minute.
    pub fn end(&mut self, grant: &Grant, unanswered: bool, now: i64) {
        let Some(waiting) = self.open.remove(grant) else {
            return;
        };
        if unanswered {
            for koid in waiting.into_iter().filter(|&k| k != 0) {
                self.refused
                    .push((koid, grant.permission.clone(), now + REFUSED_FOR));
            }
        }
    }

    /// Whether a call of `Ask` by the user and app ID at `now` is within
    /// the limit, which it counts against if it is.
    pub fn may_ask(&mut self, user: u32, app_id: &str, now: i64) -> bool {
        let times = self.calls.entry((user, app_id.into())).or_default();
        while times.front().is_some_and(|&t| t <= now - ASK_PERIOD) {
            times.pop_front();
        }
        if times.len() >= MOST_ASKS {
            return false;
        }
        times.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id<'a>(app_id: &'a str, koid: u64, checksum: &'a str) -> Identity<'a> {
        Identity {
            user: 1000,
            app_id,
            admin: 0,
            koid,
            checksum,
        }
    }

    #[test]
    fn requests_for_a_grant_wait_for_the_question_that_is_open() {
        let mut asks = Asks::default();
        let grant = Grant::of(id("org.example.app", 7, ""), "user.camera.use");
        // A second process of the same app joins the one that asked.
        let other = Grant::of(id("org.example.app", 8, ""), "user.camera.use");
        assert_eq!(grant, other);
        assert_eq!(asks.begin(&grant, 7, 0), Begin::Ask);
        assert_eq!(asks.begin(&other, 8, 5), Begin::Join);
        // Another permission, app or user has a question of its own.
        let mic = Grant::of(id("org.example.app", 7, ""), "user.microphone.use");
        assert_eq!(asks.begin(&mic, 7, 5), Begin::Ask);
        let app = Grant::of(id("org.example.other", 9, ""), "user.camera.use");
        assert_eq!(asks.begin(&app, 9, 5), Begin::Ask);
        // Answered, the next request asks again.
        asks.end(&grant, false, 10);
        assert_eq!(asks.begin(&grant, 7, 11), Begin::Ask);
    }

    #[test]
    fn the_processes_that_waited_for_an_unanswered_question_are_refused_for_a_minute() {
        let mut asks = Asks::default();
        let grant = Grant::of(id("org.example.app", 7, ""), "user.camera.use");
        assert_eq!(asks.begin(&grant, 7, 0), Begin::Ask);
        assert_eq!(asks.begin(&grant, 8, 1), Begin::Join);
        asks.end(&grant, true, 100);
        // Both are refused for the permission, until a minute is over.
        assert_eq!(asks.begin(&grant, 7, 101), Begin::Refused);
        assert_eq!(asks.begin(&grant, 8, 100 + REFUSED_FOR - 1), Begin::Refused);
        // Not for another permission, and not for another process.
        let mic = Grant::of(id("org.example.app", 7, ""), "user.microphone.use");
        assert_eq!(asks.begin(&mic, 7, 102), Begin::Ask);
        assert_eq!(asks.begin(&grant, 9, 103), Begin::Ask);
        // Over, it asks again.
        asks.end(&grant, false, 104);
        assert_eq!(asks.begin(&grant, 7, 100 + REFUSED_FOR), Begin::Ask);
    }

    #[test]
    fn an_answered_question_refuses_nobody() {
        let mut asks = Asks::default();
        let grant = Grant::of(id("org.example.app", 7, ""), "user.camera.use");
        assert_eq!(asks.begin(&grant, 7, 0), Begin::Ask);
        asks.end(&grant, false, 5);
        assert_eq!(asks.begin(&grant, 7, 6), Begin::Ask);
    }

    #[test]
    fn a_build_has_a_question_for_each_of_its_processes() {
        let mut asks = Asks::default();
        let first = Grant::of(id("org.example.app", 7, "build"), "user.camera.use");
        let second = Grant::of(id("org.example.app", 8, "build"), "user.camera.use");
        assert_ne!(first, second);
        assert_eq!(asks.begin(&first, 7, 0), Begin::Ask);
        assert_eq!(asks.begin(&second, 8, 0), Begin::Ask);
        // An installed app of the same ID has its own.
        let installed = Grant::of(id("org.example.app", 9, ""), "user.camera.use");
        assert_eq!(asks.begin(&installed, 9, 0), Begin::Ask);
    }

    #[test]
    fn a_client_without_a_process_is_never_refused() {
        let mut asks = Asks::default();
        let grant = Grant::of(id("org.example.app", 0, ""), "user.camera.use");
        assert_eq!(asks.begin(&grant, 0, 0), Begin::Ask);
        asks.end(&grant, true, 1);
        assert_eq!(asks.begin(&grant, 0, 2), Begin::Ask);
    }

    #[test]
    fn ask_is_limited_to_ten_calls_a_second_for_a_user_and_app() {
        let mut asks = Asks::default();
        for i in 0..10 {
            assert!(asks.may_ask(1000, "org.example.app", i * 10));
        }
        assert!(!asks.may_ask(1000, "org.example.app", 500));
        // Another app, or another user, has its own.
        assert!(asks.may_ask(1000, "org.example.other", 500));
        assert!(asks.may_ask(1001, "org.example.app", 500));
        // A second after the first call, one more is allowed.
        assert!(asks.may_ask(1000, "org.example.app", 1_000_000_000));
        assert!(!asks.may_ask(1000, "org.example.app", 1_000_000_001));
    }
}
