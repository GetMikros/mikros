//! Waiting for children with the process manager's `Wait`.

use crate::errno::{self, ECHILD, EINTR, EINVAL, EIO};
use crate::{clock, process};
use mikros_rt::interfaces::__midl;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{StatusKind, WaitOptions};

const WNOHANG: i64 = 1;
const WUNTRACED: i64 = 2;
const WCONTINUED: i64 = 8;
const INTERRUPTED: i32 = -27;
const FOREVER: i64 = i64::MAX;

/// `wait4`: waits for the child that `pid` selects, as `waitpid` does,
/// stores its status at `status`, and returns its process ID, or 0 when
/// `WNOHANG` finds none ended. No use of resources is reported.
pub fn wait4(pid: i64, status: *mut i32, options: i64, rusage: *mut u8) -> i64 {
    if options & !(WNOHANG | WUNTRACED | WCONTINUED) != 0 {
        return -EINVAL;
    }
    let Ok(selector) = i32::try_from(pid) else {
        return -ECHILD;
    };
    let mut wanted = 0;
    if options & WUNTRACED != 0 {
        wanted |= WaitOptions::STOPPED.0;
    }
    if options & WCONTINUED != 0 {
        wanted |= WaitOptions::CONTINUED.0;
    }
    let deadline = if options & WNOHANG != 0 {
        match clock::monotonic() {
            Ok(now) => now,
            Err(e) => return -e,
        }
    } else {
        FOREVER
    };
    let waited = process::launcher().wait(selector, WaitOptions(wanted), deadline, FOREVER);
    let ended = match waited {
        Ok(Ok(ended)) => ended,
        Ok(Err(Status::NotFound)) => return -ECHILD,
        Ok(Err(Status::TimedOut)) => return 0,
        Ok(Err(s)) => return -errno::from_status(i64::from(s.into_raw())),
        Err(__midl::Error::Status(INTERRUPTED)) => return -EINTR,
        Err(_) => return -EIO,
    };
    let s = &ended.status;
    let encoded = match s.kind {
        StatusKind::Stopped => (s.stop_signal as i32 & 0xff) << 8 | 0x7f,
        StatusKind::Continued => 0xffff,
        _ if s.term_signal != 0 => s.term_signal as i32 & 0x7f,
        _ => (s.exit_code as i32 & 0xff) << 8,
    };
    if !status.is_null() {
        // SAFETY: musl passes room for the status.
        unsafe { status.write(encoded) };
    }
    if !rusage.is_null() {
        // SAFETY: musl passes a `struct rusage`, 144 bytes on x86_64.
        unsafe { core::ptr::write_bytes(rusage, 0, 144) };
    }
    i64::from(ended.pid)
}
