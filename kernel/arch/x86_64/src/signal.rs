//! Process signals: the signal state of processes and threads, and
//! making signals pending.

use crate::process::Process;
use crate::sched::Thread;
use alloc::sync::Arc;
use kernel_core::signal::{Action, CONT, Info, Pending, SIG_MAX, discarded_by};
use kernel_core::status::Error;

/// The signal state of a process. Its lock is taken before that of any
/// of its threads' `ThreadSignals`.
pub struct ProcessSignals {
    pub actions: [Action; SIG_MAX as usize],
    pub pending: Pending,
    /// The real-time signals queued for the process and its threads.
    pub queued: usize,
}

impl Default for ProcessSignals {
    fn default() -> Self {
        ProcessSignals {
            actions: [Action::default(); SIG_MAX as usize],
            pending: Pending::default(),
            queued: 0,
        }
    }
}

/// The signal state of a thread.
#[derive(Default)]
pub struct ThreadSignals {
    pub pending: Pending,
    /// The blocked signals.
    pub mask: u64,
    /// The base and size of the alternate stack, or zeros for none.
    pub altstack: (u64, u64),
}

impl ThreadSignals {
    /// Whether `rsp` lies on the alternate stack.
    pub fn on_altstack(&self, rsp: u64) -> bool {
        let (base, size) = self.altstack;
        size != 0 && rsp.wrapping_sub(base) < size
    }
}

/// Discards the pending signals in `signals` from the process and all
/// `threads` of it. The process's signal state is locked as `state`.
pub fn discard(state: &mut ProcessSignals, threads: &[Arc<Thread>], signals: u64) {
    let ProcessSignals {
        pending, queued, ..
    } = state;
    pending.discard(signals, queued);
    for thread in threads {
        thread.signals().lock().pending.discard(signals, queued);
    }
}

/// Makes `info` pending for `process`, or for `thread`, one of its
/// threads, when given.
pub fn send(process: &Process, thread: Option<&Arc<Thread>>, info: Info) -> Result<(), Error> {
    let threads = process.live_threads();
    let mut state = process.signals().lock();
    discard(&mut state, &threads, discarded_by(info.signo));
    let ProcessSignals {
        pending, queued, ..
    } = &mut *state;
    match thread {
        Some(thread) => thread.signals().lock().pending.add(info, queued)?,
        None => pending.add(info, queued)?,
    }
    drop(state);
    if info.signo == CONT {
        process.continue_threads();
    }
    Ok(())
}

/// The union of the signals pending for `thread` and for its process.
pub fn pending_mask(process: &Process, thread: &Thread) -> u64 {
    let state = process.signals().lock();
    state.pending.mask() | thread.signals().lock().pending.mask()
}
