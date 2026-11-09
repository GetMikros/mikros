//! Process signals: the signal state of processes and threads, and
//! making signals pending.

mod set;
pub use set::*;

use crate::object::budget::{Budget, Scratch};
use crate::process::Process;
use crate::sched::{self, Thread};
use crate::status::Error;
use alloc::sync::Arc;
use set::Default as DefaultAction;

/// The signal state of a process. Its lock is taken before that of any
/// of its threads' `ThreadSignals`.
pub struct ProcessSignals {
    pub actions: [Action; SIG_MAX as usize],
    pub pending: Pending,
    /// The real-time signals queued for the process and its threads.
    pub queued: usize,
}

impl core::default::Default for ProcessSignals {
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
    /// The temporary mask that `signal_set_wait_mask` gave for the next
    /// system call.
    pub wait_mask: Option<u64>,
    /// The mask that a temporary mask replaced for the system call that
    /// runs, to be restored when it returns or saved in its handler's
    /// frame.
    pub saved_mask: Option<u64>,
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
/// threads, when given. A queued real-time signal is charged to `budget`,
/// the sender's.
pub fn send(
    process: &Process,
    thread: Option<&Arc<Thread>>,
    info: Info,
    budget: &Arc<Budget>,
) -> Result<(), Error> {
    let threads = process.live_threads(&mut Scratch::new(Some(budget)))?;
    let mut state = process.signals().lock();
    discard(&mut state, &threads, discarded_by(info.signo));
    let ProcessSignals {
        pending, queued, ..
    } = &mut *state;
    match thread {
        // A thread that ended has discarded what was pending for it,
        // under this lock, so nothing may be added for it now.
        Some(thread) if thread.state() == sched::DEAD => return Err(Error::BadState),
        Some(thread) => thread
            .signals()
            .lock()
            .pending
            .add(info, queued, Some(budget))?,
        None => pending.add(info, queued, Some(budget))?,
    }
    drop(state);
    let targets = threads
        .iter()
        .filter(|t| thread.is_none_or(|target| Arc::ptr_eq(t, target)));
    // Waking a thread that has not slept yet leaves it a wake that its
    // next sleep consumes, so a wait that checked for signals just before
    // this send does not sleep through it; a thread running on another
    // processor is interrupted and takes the signal on its way back to
    // user mode.
    for target in targets {
        sched::wake_sleeper(target);
    }
    if info.signo == CONT {
        process.continue_threads();
    } else if info.signo == KILL {
        process.wake_stopped();
    }
    Ok(())
}

/// Discards the signals pending for `thread`, which has ended, so that
/// the queued real-time ones no longer count toward its process's limit.
pub fn thread_ended(thread: &Thread) {
    let Some(process) = thread.process() else {
        return;
    };
    let mut state = process.signals().lock();
    thread
        .signals()
        .lock()
        .pending
        .discard(!0, &mut state.queued);
}

/// The union of the signals pending for `thread` and for its process.
pub fn pending_mask(process: &Process, thread: &Thread) -> u64 {
    let state = process.signals().lock();
    state.pending.mask() | thread.signals().lock().pending.mask()
}

/// The lowest signal pending for the thread with `own` signal state or
/// its process that it does not block and for which `ends` holds.
fn first_deliverable(
    pending: &Pending,
    own: &ThreadSignals,
    ends: impl Fn(u32) -> bool,
) -> Option<u32> {
    let deliverable = (own.pending.mask() | pending.mask()) & !own.mask;
    (1..=SIG_MAX)
        .filter(|&s| deliverable & bit(s) != 0)
        .find(|&s| ends(s))
}

/// Whether the thread with `own` signal state ends its call for `signo`,
/// found by `first_deliverable`. A signal pending for the process moves
/// to the thread first, so that no other thread ends a call for it too.
fn claim(pending: &mut Pending, own: &mut ThreadSignals, signo: u32) -> bool {
    own.pending.mask() & bit(signo) != 0 || pending.move_one(signo, &mut own.pending)
}

/// Whether a signal that would run an action is pending for `thread` of
/// `process`, so that a blocking call must end with `Interrupted`.
pub fn interrupted(thread: &Thread, process: &Process) -> bool {
    let mut state = process.signals().lock();
    interrupts(&mut state, &mut thread.signals().lock())
}

/// `interrupted` with the signal state of the process locked as `state`
/// and that of the thread as `own`.
fn interrupts(state: &mut ProcessSignals, own: &mut ThreadSignals) -> bool {
    let ProcessSignals {
        actions, pending, ..
    } = state;
    let first = first_deliverable(pending, own, |s| !actions[s as usize - 1].ignores(s));
    first.is_some_and(|s| claim(pending, own, s))
}

/// Whether a signal pending for `thread` of `process` ends a
/// `channel_call` it waits in: one that terminates the process, or a
/// handler without `RESTART`. A handler with `RESTART` lets the call go
/// on, and runs once it returns.
pub fn call_interrupted(thread: &Thread, process: &Process) -> bool {
    let mut state = process.signals().lock();
    let mut own = thread.signals().lock();
    let ProcessSignals {
        actions, pending, ..
    } = &mut *state;
    let ends = |s: u32| {
        let action = &actions[s as usize - 1];
        match action.handler {
            HANDLER_IGNORE => false,
            HANDLER_DEFAULT => default_action(s) == DefaultAction::Terminate,
            _ => action.flags & RESTART == 0,
        }
    };
    let first = first_deliverable(pending, &own, ends);
    first.is_some_and(|s| claim(pending, &mut own, s))
}

/// Blocks the running thread until `deadline`, or fails with
/// `Interrupted` once a signal that would run an action is pending for it.
pub fn sleep(deadline: i64) -> Result<(), Error> {
    let thread = sched::current().ok_or(Error::BadHandle)?;
    let process = thread.process().cloned().ok_or(Error::BadHandle)?;
    loop {
        if interrupted(&thread, &process) || thread.leave_wait_to_suspend() {
            return Err(Error::Interrupted);
        }
        if thread.state() == sched::DEAD || deadline <= crate::arch::monotonic_now() {
            return Ok(());
        }
        sched::wait_for_signals(&thread, deadline)?;
    }
}

/// Waits until `deadline` for a signal in `mask` to be pending for the
/// running thread or its process, leaving it pending, so that the caller
/// can prepare its output before `take_pending`.
pub fn wait_pending(mask: u64, deadline: i64) -> Result<(), Error> {
    let thread = sched::current().ok_or(Error::BadHandle)?;
    let process = thread.process().cloned().ok_or(Error::BadHandle)?;
    let mask = mask & !UNBLOCKABLE;
    loop {
        {
            let mut state = process.signals().lock();
            let mut own = thread.signals().lock();
            if (own.pending.mask() | state.pending.mask()) & mask != 0 {
                return Ok(());
            }
            if interrupts(&mut state, &mut own) {
                return Err(Error::Interrupted);
            }
        }
        if thread.leave_wait_to_suspend() {
            return Err(Error::Interrupted);
        }
        if thread.state() == sched::DEAD || deadline <= crate::arch::monotonic_now() {
            return Err(Error::TimedOut);
        }
        sched::wait_for_signals(&thread, deadline)?;
    }
}

/// Removes the pending signal in `mask` that `signal_wait` returns, the
/// running thread's before its process's, if any.
pub fn take_pending(mask: u64) -> Option<Info> {
    let thread = sched::current()?;
    let process = thread.process().cloned()?;
    let mask = mask & !UNBLOCKABLE;
    let mut state = process.signals().lock();
    let ProcessSignals {
        pending, queued, ..
    } = &mut *state;
    let mut own = thread.signals().lock();
    own.pending
        .take(mask, queued)
        .or_else(|| pending.take(mask, queued))
}

/// Makes an exception of type `kind` at `address`, raised by the running
/// thread, pending for it as its signal, or terminates the process when
/// the thread blocks that signal or ignores it. The signal is delivered
/// before the thread returns to user mode.
pub fn exception_signal(process: &Process, thread: &Arc<Thread>, kind: u32, address: u64) {
    let signo = for_exception(kind);
    let info = Info {
        signo,
        code: CODE_EXCEPTION,
        address,
        exception_type: kind,
        ..Info::default()
    };
    let refused = {
        let mut state = process.signals().lock();
        let ignored = state.actions[signo as usize - 1].ignores(signo);
        let ProcessSignals { queued, .. } = &mut *state;
        let mut own = thread.signals().lock();
        let blocked = own.mask & bit(signo) != 0;
        // Exceptions raise standard signals, which are never charged.
        ignored || blocked || own.pending.add(info, queued, None).is_err()
    };
    if refused {
        process.terminate_by_signal(signo);
    }
}
