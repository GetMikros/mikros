//! The state of a pending entry's gate (MIKSPEC-0094.R11), apart from the
//! protocol that carries it: for one entry and one subject, which of the
//! calls that need an answer asks, which wait for that question, and
//! what each is told when it ends or when the grant is revoked.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// What a pending entry shows: a folder of the user's home, or the
/// volumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Folder(&'static str),
    Volumes,
}

/// The permissions that get a pending entry (MIKSPEC-0094.R10), and what
/// each shows.
pub const PENDING: [(&str, Target); 8] = [
    ("user.desktop.access", Target::Folder("desktop")),
    ("user.documents.access", Target::Folder("documents")),
    ("user.downloads.access", Target::Folder("downloads")),
    ("user.pictures.access", Target::Folder("pictures")),
    ("user.music.access", Target::Folder("music")),
    ("user.videos.access", Target::Folder("videos")),
    ("user.developer.access", Target::Folder("developer")),
    ("user.volumes.access", Target::Volumes),
];

/// What a call that needs an answer does on arriving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrival {
    /// No question is open: the caller makes the `Request` and calls
    /// `ended` with its answer. The call waits for it.
    Ask,
    /// A `Request` is open: the call waits for its answer.
    Wait,
    /// The answer was `ALLOWED`: the call goes on to the folder.
    Proceed,
    /// The entry was revoked: the call fails with `ACCESS_DENIED`.
    Refuse,
}

/// What `Request` came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer<E> {
    /// `ALLOWED`, and the folder is open.
    Allowed,
    /// Any other decision.
    Denied,
    /// `Request` failed, or the folder could not be opened.
    Failed(E),
}

/// What becomes of a waiting call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome<E> {
    Proceed,
    /// The call fails with `ACCESS_DENIED`.
    Denied,
    /// The call fails with the status of the failure.
    Failed(E),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Nothing is known: the next call asks.
    Idle,
    /// A `Request` is open.
    Asking,
    Allowed,
    /// Revoked for the life of the process, which is never asked again.
    Revoked,
}

/// The state of one entry for one subject. `W` names a waiting call.
#[derive(Debug)]
pub struct Machine<W> {
    phase: Phase,
    /// Whether it was `ALLOWED` once, so that it forwarded calls.
    forwarded: bool,
    waiting: VecDeque<W>,
}

impl<W> Machine<W> {
    /// A machine for a subject that is bound after the entry was
    /// `revoked`, or not.
    pub fn new(revoked: bool) -> Machine<W> {
        Machine {
            phase: if revoked { Phase::Revoked } else { Phase::Idle },
            forwarded: false,
            waiting: VecDeque::new(),
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Whether the four calls that need no answer are still answered as
    /// before an answer: they are until the gate forwards, and not once it
    /// was revoked after that (MIKSPEC-0094.R11).
    pub fn unanswered(&self) -> bool {
        !self.forwarded && self.phase != Phase::Allowed
    }

    /// Whether the four calls that need no answer are forwarded now.
    pub fn allowed(&self) -> bool {
        self.phase == Phase::Allowed
    }

    /// A call that needs an answer arrives as `call`; it is kept only
    /// when it has to wait.
    pub fn arrive(&mut self, call: W) -> Arrival {
        match self.phase {
            Phase::Idle => {
                self.phase = Phase::Asking;
                self.waiting.push_back(call);
                Arrival::Ask
            }
            Phase::Asking => {
                self.waiting.push_back(call);
                Arrival::Wait
            }
            Phase::Allowed => Arrival::Proceed,
            Phase::Revoked => Arrival::Refuse,
        }
    }

    /// The open question ended with `answer`: what becomes of each call
    /// that waited for it, in the order they arrived. An answer that
    /// ends a question after the revocation changes nothing.
    pub fn ended<E: Clone>(&mut self, answer: Answer<E>) -> Vec<(W, Outcome<E>)> {
        if self.phase != Phase::Asking {
            return Vec::new();
        }
        let (phase, outcome) = match answer {
            Answer::Allowed => (Phase::Allowed, Outcome::Proceed),
            Answer::Denied => (Phase::Idle, Outcome::Denied),
            Answer::Failed(e) => (Phase::Idle, Outcome::Failed(e)),
        };
        self.phase = phase;
        self.forwarded |= phase == Phase::Allowed;
        self.waiting
            .drain(..)
            .map(|w| (w, outcome.clone()))
            .collect()
    }

    /// The grant was revoked: every waiting call fails, and none is ever
    /// asked for again.
    pub fn revoke<E>(&mut self) -> Vec<(W, Outcome<E>)> {
        self.phase = Phase::Revoked;
        self.waiting
            .drain(..)
            .map(|w| (w, Outcome::Denied))
            .collect()
    }
}

#[cfg(test)]
mod tests;
