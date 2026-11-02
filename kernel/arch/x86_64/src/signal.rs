//! Process signals: the signal state of processes and threads, and
//! making signals pending.

use crate::aspace::in_user_range;
use crate::idt::Frame;
use crate::process::{Process, STOPPED};
use crate::sched::{self, Thread};
use crate::thread_state::{self, GENERAL_SIZE, General};
use crate::usercopy::{copy_in, copy_out};
use alloc::sync::Arc;
use alloc::vec::Vec;
use kernel_core::signal::{
    Action, CODE_EXCEPTION, CONT, Default as DefaultAction, HANDLER_DEFAULT, HANDLER_IGNORE,
    INFO_SIZE, Info, KILL, NODEFER, ONSTACK, Pending, RESETHAND, RESTART, SEGV, SIG_MAX,
    UNBLOCKABLE, bit, default_action, discarded_by, for_exception,
};
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
    let waiting = threads.iter().filter(|t| t.waits_for_signals());
    for waiter in waiting.filter(|t| thread.is_none_or(|target| Arc::ptr_eq(t, target))) {
        sched::wake_sleeper(waiter);
    }
    if info.signo == CONT {
        process.continue_threads();
    } else if info.signo == KILL {
        process.wake_stopped();
    }
    Ok(())
}

/// The union of the signals pending for `thread` and for its process.
pub fn pending_mask(process: &Process, thread: &Thread) -> u64 {
    let state = process.signals().lock();
    state.pending.mask() | thread.signals().lock().pending.mask()
}

/// The bytes below the interrupted stack pointer that a frame on the
/// current stack leaves alone.
const RED_ZONE: u64 = 128;
/// The offset of the extended state from the context address.
const XSAVE_OFFSET: u64 = 0xC0;
/// The bytes below the context address: the restorer and the record.
const BELOW_CONTEXT: u64 = 72;

/// The context address of a frame of `size` bytes from the context
/// address on: the highest 64-byte-aligned one at which the frame ends at
/// or below `end`.
fn context_address(end: u64, size: u64) -> Option<u64> {
    Some(end.checked_sub(size)? & !63)
}

/// Delivers the pending signals that the running thread does not block,
/// as it returns to user mode with its registers in `frame`, and keeps it
/// here while its process is stopped. Returns whether `frame` changed.
pub fn deliver(frame: &mut Frame) -> bool {
    let mut changed = false;
    loop {
        let Some(thread) = sched::current() else {
            return changed;
        };
        let Some(process) = thread.process().cloned() else {
            return changed;
        };
        if thread.state() == sched::DEAD {
            return changed;
        }
        let stopped = process.state() == STOPPED;
        let next = {
            let mut state = process.signals().lock();
            let ProcessSignals {
                actions,
                pending,
                queued,
            } = &mut *state;
            let mut own = thread.signals().lock();
            // A stopped process acts only on KILL; CONT has already let
            // it run when it was sent.
            let allowed = if stopped { bit(KILL) } else { !own.mask };
            let info = own
                .pending
                .take(allowed, queued)
                .or_else(|| pending.take(allowed, queued));
            info.map(|info| (info, actions[info.signo as usize - 1]))
        };
        let Some((info, action)) = next else {
            if stopped {
                process.stop_here(&thread);
                continue;
            }
            thread.take_interrupted();
            return changed;
        };
        match action.handler {
            HANDLER_IGNORE => {}
            HANDLER_DEFAULT => match default_action(info.signo) {
                DefaultAction::Ignore | DefaultAction::Continue => {}
                DefaultAction::Terminate => {
                    process.terminate_by_signal(info.signo);
                    return changed;
                }
                DefaultAction::Stop => process.stop(),
            },
            _ => {
                if write_frame(frame, &thread, &process, &info, &action).is_err() {
                    process.terminate_by_signal(SEGV);
                    return changed;
                }
                changed = true;
            }
        }
    }
}

/// Writes a signal frame for `info` with `action` onto the stack of
/// `thread`, whose registers are in `frame`, and points the registers at
/// the handler. Fails when the frame cannot be written.
fn write_frame(
    frame: &mut Frame,
    thread: &Thread,
    process: &Process,
    info: &Info,
    action: &Action,
) -> Result<(), Error> {
    let extended = thread.extended_state();
    extended.save();
    let xsave = extended.bytes();
    let size = XSAVE_OFFSET + xsave.len() as u64;
    let mut own = thread.signals().lock();
    let alternate =
        action.flags & ONSTACK != 0 && own.altstack.1 != 0 && !own.on_altstack(frame.rsp);
    let end = if alternate {
        own.altstack.0 + own.altstack.1
    } else {
        frame.rsp.checked_sub(RED_ZONE).ok_or(Error::InvalidArgs)?
    };
    let context = context_address(end, size).ok_or(Error::InvalidArgs)?;
    let bottom = context
        .checked_sub(BELOW_CONTEXT)
        .ok_or(Error::InvalidArgs)?;
    if !in_user_range(bottom, size + BELOW_CONTEXT) {
        return Err(Error::InvalidArgs);
    }
    // A system call this signal interrupted runs again after the handler
    // returns when the action asks for it.
    if let Some(number) = thread.take_interrupted()
        && action.flags & RESTART != 0
    {
        frame.rip -= SYSCALL_LEN;
        frame.rax = number;
    }
    let (fs, gs) = sched::user_bases();
    let general = General::from_frame(frame, fs, gs);
    let mut head = [0u8; (BELOW_CONTEXT + XSAVE_OFFSET) as usize];
    head[..8].copy_from_slice(&action.restorer.to_le_bytes());
    head[8..8 + INFO_SIZE].copy_from_slice(&info.bytes());
    let c = BELOW_CONTEXT as usize;
    head[c..c + GENERAL_SIZE].copy_from_slice(&general.bytes());
    head[c + 0xA0..c + 0xA8].copy_from_slice(&own.mask.to_le_bytes());
    head[c + 0xA8..c + 0xB0].copy_from_slice(&(xsave.len() as u64).to_le_bytes());
    copy_out(bottom, &head)?;
    copy_out(context + XSAVE_OFFSET, xsave)?;
    own.mask |= action.mask & !UNBLOCKABLE;
    if action.flags & NODEFER == 0 {
        own.mask |= bit(info.signo) & !UNBLOCKABLE;
    }
    drop(own);
    if action.flags & RESETHAND != 0 {
        process.signals().lock().actions[info.signo as usize - 1] = Action::default();
    }
    frame.rsp = bottom;
    frame.rip = action.handler;
    frame.rdi = u64::from(info.signo);
    frame.rsi = context - (BELOW_CONTEXT - 8);
    frame.rdx = context;
    frame.rflags &= !RFLAGS_DF;
    Ok(())
}

const RFLAGS_DF: u64 = 1 << 10;
/// The length of the SYSCALL instruction.
const SYSCALL_LEN: u64 = 2;

/// Waits until a signal in `mask` is pending for the running thread or
/// its process, and takes it. Fails with `Interrupted` when a signal
/// outside `mask` that is not ignored becomes deliverable first, and with
/// `TimedOut` when `deadline` passes.
/// Whether a signal pending for the thread with `own` signal state or its
/// process is unmasked and not ignored, so that a blocking call must end.
fn interrupting(actions: &[Action], pending: &Pending, own: &ThreadSignals) -> bool {
    let deliverable = (own.pending.mask() | pending.mask()) & !own.mask;
    (1..=SIG_MAX)
        .filter(|&s| deliverable & bit(s) != 0)
        .any(|s| !actions[s as usize - 1].ignores(s))
}

/// Blocks the running thread until `deadline`, or fails with
/// `Interrupted` once a signal that would run an action is pending for it.
pub fn sleep(deadline: i64) -> Result<(), Error> {
    let thread = sched::current().ok_or(Error::BadHandle)?;
    let process = thread.process().cloned().ok_or(Error::BadHandle)?;
    loop {
        {
            let state = process.signals().lock();
            let own = thread.signals().lock();
            if interrupting(&state.actions, &state.pending, &own) {
                return Err(Error::Interrupted);
            }
        }
        if thread.state() == sched::DEAD || deadline <= crate::time::now() {
            return Ok(());
        }
        sched::wait_for_signals(&thread, deadline)?;
    }
}

pub fn wait(mask: u64, deadline: i64) -> Result<Info, Error> {
    let thread = sched::current().ok_or(Error::BadHandle)?;
    let process = thread.process().cloned().ok_or(Error::BadHandle)?;
    let mask = mask & !UNBLOCKABLE;
    loop {
        {
            let mut state = process.signals().lock();
            let ProcessSignals {
                actions,
                pending,
                queued,
            } = &mut *state;
            let mut own = thread.signals().lock();
            let taken = own
                .pending
                .take(mask, queued)
                .or_else(|| pending.take(mask, queued));
            if let Some(info) = taken {
                return Ok(info);
            }
            if interrupting(actions, pending, &own) {
                return Err(Error::Interrupted);
            }
        }
        if thread.state() == sched::DEAD || deadline <= crate::time::now() {
            return Err(Error::TimedOut);
        }
        sched::wait_for_signals(&thread, deadline)?;
    }
}

/// Restores the registers, the extended state and the mask of the
/// running thread from the context at `context`, into `frame`. A context
/// that cannot be read or is not allowed terminates the process as `SEGV`
/// would.
pub fn sigreturn(frame: &mut Frame, context: u64) {
    let Some(thread) = sched::current() else {
        return;
    };
    let Some(process) = thread.process().cloned() else {
        return;
    };
    if restore(frame, &thread, context).is_err() {
        process.terminate_by_signal(SEGV);
    }
}

fn restore(frame: &mut Frame, thread: &Thread, context: u64) -> Result<(), Error> {
    let mut head = [0u8; XSAVE_OFFSET as usize];
    copy_in(&mut head, context)?;
    let general: &[u8; GENERAL_SIZE] = head[..GENERAL_SIZE].try_into().expect("160 bytes");
    let general = General::parse(general, |a| in_user_range(a, 1)).ok_or(Error::InvalidArgs)?;
    let mask = u64::from_le_bytes(head[0xA0..0xA8].try_into().expect("8 bytes"));
    let size = u64::from_le_bytes(head[0xA8..0xB0].try_into().expect("8 bytes"));
    let extended = thread.extended_state();
    if size != extended.bytes().len() as u64 {
        return Err(Error::InvalidArgs);
    }
    let mut xsave = Vec::new();
    xsave
        .try_reserve_exact(size as usize)
        .map_err(|_| Error::NoMemory)?;
    xsave.resize(size as usize, 0);
    copy_in(&mut xsave, context + XSAVE_OFFSET)?;
    extended.save();
    let mxcsr_mask = thread_state::mxcsr_mask(extended.bytes());
    if !thread_state::xsave_loadable(&xsave, crate::context::xcr0(), mxcsr_mask) {
        return Err(Error::InvalidArgs);
    }
    extended.bytes_mut().copy_from_slice(&xsave);
    extended.restore();
    general.apply(frame);
    sched::set_user_base(sched::UserBase::Fs, general.fs_base);
    sched::set_user_base(sched::UserBase::Gs, general.gs_base);
    thread.signals().lock().mask = mask & !UNBLOCKABLE;
    Ok(())
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
        ignored || blocked || own.pending.add(info, queued).is_err()
    };
    if refused {
        process.terminate_by_signal(signo);
    }
}
