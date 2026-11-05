//! The process's IDs and `kill`, which sends signals through the process
//! manager's `Launcher`.

use crate::errno::{self, EINVAL, EIO, ENOSYS, EPERM, ESRCH};
use crate::lock::Lock;
use core::mem::ManuallyDrop;
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::LauncherClient;
use mikros_rt::sys::{self, Handle};

const FOREVER: i64 = i64::MAX;

static PID: AtomicU32 = AtomicU32::new(0);
static PARENT: AtomicU32 = AtomicU32::new(0);
/// The `Launcher` channel of the startup transfer, which carries one
/// call at a time.
static LAUNCHER: Lock<Handle> = Lock::new(0);

/// Keeps the IDs and the `Launcher` channel of the startup transfer.
pub fn install(pid: u32, parent: u32, launcher: Handle) {
    PID.store(pid, Ordering::Relaxed);
    PARENT.store(parent, Ordering::Relaxed);
    *LAUNCHER.lock() = launcher;
}

pub fn getpid() -> i64 {
    i64::from(PID.load(Ordering::Relaxed))
}

pub fn getppid() -> i64 {
    i64::from(PARENT.load(Ordering::Relaxed))
}

/// The error number of a failed `Kill`: those of POSIX `kill` for a
/// process that does not exist or may not be signalled.
fn kill_error(status: Status) -> i64 {
    match status {
        Status::NotFound => ESRCH,
        Status::AccessDenied => EPERM,
        status => errno::from_status(i64::from(status.into_raw())),
    }
}

/// `kill`: sends `signo` to process `pid`, or to process group `-pid`
/// below -1. Signalling every process, or the caller's group, is not
/// supported yet.
pub fn kill(pid: i64, signo: i64) -> i64 {
    if !(0..=64).contains(&signo) {
        return -EINVAL;
    }
    let group = match pid {
        1.. => None,
        ..-1 => match u32::try_from(-pid) {
            Ok(group) => Some(group),
            Err(_) => return -ESRCH,
        },
        _ => return -ENOSYS,
    };
    let launcher = LAUNCHER.lock();
    // SAFETY: the channel stays open for as long as the process runs; the
    // client is never dropped, so it never closes it.
    let client = ManuallyDrop::new(LauncherClient::new(ClientEnd::new(unsafe {
        OwnedHandle::from_raw(*launcher)
    })));
    let signo = signo as u32;
    // A signal to the caller would interrupt the call; it is blocked
    // until the call returns, and delivered as the mask is restored.
    let mut mask = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::signal_set_mask(0, u64::MAX, &mut mask) };
    let result = match group {
        None => client.kill(pid as u32, signo, FOREVER).map(|r| r.map(drop)),
        Some(group) => client
            .kill_group(group, signo, FOREVER)
            .map(|r| r.map(drop)),
    };
    // SAFETY: the old mask may be null.
    unsafe { sys::signal_set_mask(2, mask, core::ptr::null_mut()) };
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(status)) => -kill_error(status),
        Err(_) => -EIO,
    }
}
