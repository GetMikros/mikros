//! Delivering signals on x86_64: the signal frame on the user stack,
//! and restoring the registers from it with `signal_return`.

use crate::aspace::in_user_range;
use crate::idt::Frame;
use crate::process::{Process, STOPPED};
use crate::sched::{self, Thread};
use crate::signal::ProcessSignals;
use crate::thread_arch::ThreadX86;
use crate::thread_state::{self, GENERAL_SIZE, General};
use crate::usercopy::{copy_in, copy_out};
use alloc::vec::Vec;
use kernel_core::object::budget::Scratch;
use kernel_core::signal::{
    Action, Default as DefaultAction, HANDLER_DEFAULT, HANDLER_IGNORE, INFO_SIZE, Info, KILL,
    NODEFER, ONSTACK, RESETHAND, RESTART, SEGV, UNBLOCKABLE, bit, default_action,
};
use kernel_core::status::Error;

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
            let next = info.map(|info| (info, actions[info.signo as usize - 1]));
            // A stop takes effect before the lock goes, so that a CONT
            // sent once it is released finds the process stopped.
            let stops = next.is_some_and(|(info, action)| {
                action.handler == HANDLER_DEFAULT
                    && default_action(info.signo) == DefaultAction::Stop
            });
            if stops && process.stop() {
                drop(own);
                drop(state);
                process.notify_threads();
            }
            next
        };
        let Some((info, action)) = next else {
            // A thread that is to end leaves the stop to report its exit,
            // and one that is to be suspended to be suspended.
            if stopped && !thread.exit_requested() && !thread.suspend_pending() {
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
                // The process stopped above.
                DefaultAction::Stop => {}
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

/// Resets the action of `signo` in `process` to the default action for
/// `RESETHAND`, discarding the pending instances of `signo` when the
/// default ignores it. When the list of threads cannot be made, only
/// those pending for the process and for `thread`, the running one, are
/// discarded.
fn reset_action(process: &Process, thread: &Thread, signo: u32) {
    let ignores = Action::default().ignores(signo);
    let threads = match ignores {
        true => process
            .live_threads(&mut Scratch::new(Some(process.budget())))
            .unwrap_or_default(),
        false => Vec::new(),
    };
    let mut state = process.signals().lock();
    state.actions[signo as usize - 1] = Action::default();
    if ignores {
        kernel_core::signal::discard(&mut state, &threads, bit(signo));
        let ProcessSignals { queued, .. } = &mut *state;
        thread.signals().lock().pending.discard(bit(signo), queued);
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
    let extended = thread.x86().extended_state();
    extended.save();
    let xsave = extended.bytes();
    let size = XSAVE_OFFSET + xsave.len() as u64;
    // Only the thread itself changes its mask and alternate stack, so
    // they stay as read here while the frame is written without the lock,
    // which a fault on the user stack may wait with.
    let (altstack, on_altstack, mask) = {
        let own = thread.signals().lock();
        (own.altstack, own.on_altstack(frame.rsp), own.mask)
    };
    let alternate = action.flags & ONSTACK != 0 && altstack.1 != 0 && !on_altstack;
    let end = if alternate {
        altstack.0 + altstack.1
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
    let (fs, gs) = crate::thread_arch::user_bases();
    let general = General::from_frame(frame, fs, gs);
    let mut head = [0u8; (BELOW_CONTEXT + XSAVE_OFFSET) as usize];
    head[..8].copy_from_slice(&action.restorer.to_le_bytes());
    head[8..8 + INFO_SIZE].copy_from_slice(&info.bytes());
    let c = BELOW_CONTEXT as usize;
    head[c..c + GENERAL_SIZE].copy_from_slice(&general.bytes());
    head[c + 0xA0..c + 0xA8].copy_from_slice(&mask.to_le_bytes());
    head[c + 0xA8..c + 0xB0].copy_from_slice(&(xsave.len() as u64).to_le_bytes());
    copy_out(bottom, &head)?;
    copy_out(context + XSAVE_OFFSET, xsave)?;
    {
        let mut own = thread.signals().lock();
        own.mask |= action.mask & !UNBLOCKABLE;
        if action.flags & NODEFER == 0 {
            own.mask |= bit(info.signo) & !UNBLOCKABLE;
        }
    }
    if action.flags & RESETHAND != 0 {
        reset_action(process, thread, info.signo);
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
    // The bytes between the size and the extended state are reserved.
    if head[0xB0..].iter().any(|&b| b != 0) {
        return Err(Error::InvalidArgs);
    }
    let extended = thread.x86().extended_state();
    if size != extended.bytes().len() as u64 {
        return Err(Error::InvalidArgs);
    }
    let mut xsave = Vec::new();
    let mut scratch = Scratch::new(thread.process().map(|p| p.budget()));
    scratch.reserve_exact(&mut xsave, size as usize)?;
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
    crate::thread_arch::set_user_base(crate::thread_arch::UserBase::Fs, general.fs_base);
    crate::thread_arch::set_user_base(crate::thread_arch::UserBase::Gs, general.gs_base);
    thread.signals().lock().mask = mask & !UNBLOCKABLE;
    Ok(())
}
