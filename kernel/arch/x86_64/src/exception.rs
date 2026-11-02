//! Reporting exceptions of user threads on their process's exception
//! channel.

use crate::idt::{self, Frame};
use crate::sched;
use alloc::vec::Vec;
use kernel_core::exception::{EXIT_CODE, REPORT_SIZE, Report};
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
    let Some(thread) = sched::current() else {
        crate::panic::exception(frame);
    };
    let Some(process) = thread.process().cloned() else {
        crate::panic::exception(frame);
    };
    // Process signals take exceptions that no channel handles; until
    // they exist, such an exception stops the kernel as before.
    let Some(channel) = process.exception_channel() else {
        crate::panic::exception(frame);
    };
    let report = Report {
        kind,
        process_koid: process.header().koid(),
        thread_koid: thread.header().koid(),
        address,
        arch_code: idt::arch_code(frame),
    };
    sched::enter_exception(&thread, frame);
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
    if sched::wait_exception(&thread) == sched::HANDLER_GONE {
        process.terminate(EXIT_CODE);
    }
}
