//! Reporting exceptions of user threads on their process's exception
//! channel.

use crate::idt::{self, Frame};
use crate::process::Process;
use crate::sched;
use alloc::vec::Vec;
use kernel_core::exception::{EXIT_CODE, REPORT_SIZE, Report, kind};
use kernel_core::object::channel::Message;
use kernel_core::object::handles::Entry;
use kernel_core::object::rights::{DUPLICATE, INSPECT, MANAGE, TRANSFER, WAIT};
use kernel_core::object::{Object, rights};
use kernel_core::status::Error;

/// The rights of the thread handle in a report.
const REPORT_RIGHTS: u32 = DUPLICATE | TRANSFER | WAIT | MANAGE | INSPECT;

const _: () = assert!(REPORT_RIGHTS & !rights::ALL == 0);

/// Reports an exception of type `kind` at `address` that the running
/// user thread raised, with its user registers in `frame`, and waits
/// until the handler resumes the thread. Returns with the thread
/// RUNNABLE or DEAD; the caller ends it if it is DEAD.
pub fn raise(frame: &mut Frame, kind: u32, address: u64) {
    raise_with_code(frame, kind, address, idt::arch_code(frame));
}

/// Reports a thread start or exit event of type `kind` at `address` for
/// the running thread, with its user registers in `frame`.
pub fn report_event(frame: &mut Frame, kind: u32, address: u64) {
    raise_with_code(frame, kind, address, 0);
}

/// Makes the reports that wait for the running user thread to return to
/// user mode with its registers in `frame`: a debug exception its
/// breakpoints raised in kernel mode, and its exit when it is to end.
/// Returns whether it reported anything.
pub fn before_user_return(frame: &mut Frame) -> bool {
    let Some(thread) = sched::current() else {
        return false;
    };
    let mut reported = false;
    if thread.take_deferred_debug() && thread.state() != sched::DEAD {
        let rip = frame.rip;
        raise_with_code(frame, kind::DEBUG, rip, u64::from(idt::DEBUG));
        reported = true;
    }
    reported |= end_if_requested(frame, &thread);
    if let Some(context) = thread.take_sigreturn() {
        crate::signal::sigreturn(frame, context);
        reported = true;
    }
    reported |= crate::signal::deliver(frame);
    // A stop may have held the thread when it was asked to end.
    reported | end_if_requested(frame, &thread)
}

/// Ends the running `thread`, with its user registers in `frame`, when
/// it is to end, after a `THREAD_EXITING` report when its process asks
/// for thread events. Returns whether it reported.
fn end_if_requested(frame: &mut Frame, thread: &alloc::sync::Arc<sched::Thread>) -> bool {
    if !thread.take_exit_request() {
        return false;
    }
    let process = thread.process().cloned();
    let report = process.is_some_and(|p| p.debug_events()) && thread.state() != sched::DEAD;
    if report {
        raise_with_code(frame, kind::THREAD_EXITING, 0, 0);
    }
    Process::kill_thread(thread);
    report
}

fn raise_with_code(frame: &mut Frame, kind: u32, address: u64, arch_code: u64) {
    let Some(thread) = sched::current() else {
        crate::panic::exception(frame);
    };
    let Some(process) = thread.process().cloned() else {
        crate::panic::exception(frame);
    };
    let Some(channel) = process.exception_channel() else {
        // Thread events are reported only on a channel.
        if kind == kind::THREAD_STARTING || kind == kind::THREAD_EXITING {
            return;
        }
        crate::signal::exception_signal(&process, &thread, kind, address);
        return;
    };
    let report = Report {
        kind,
        process_koid: process.header().koid(),
        thread_koid: thread.header().koid(),
        address,
        arch_code,
    };
    if !sched::enter_exception(&thread, frame, kind) {
        return;
    }
    let (mut bytes, mut handles) = (Vec::new(), Vec::new());
    let queued = bytes
        .try_reserve_exact(REPORT_SIZE)
        .and_then(|()| handles.try_reserve_exact(1))
        .map_err(|_| Error::NoMemory)
        .and_then(|()| {
            bytes.extend_from_slice(&report.bytes());
            handles.push(Entry {
                object: thread.clone(),
                rights: REPORT_RIGHTS,
            });
            Message::new(bytes, handles, None)
        })
        .and_then(|message| channel.write(message).map_err(|(e, _)| e));
    drop(channel);
    if queued.is_err() {
        sched::leave_exception(&thread);
        process.terminate(EXIT_CODE);
        return;
    }
    match sched::wait_exception(&thread) {
        sched::HANDLER_GONE => process.terminate(EXIT_CODE),
        sched::DELIVER_SIGNAL => crate::signal::exception_signal(&process, &thread, kind, address),
        _ => {}
    }
}
