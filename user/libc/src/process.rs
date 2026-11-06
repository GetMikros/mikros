//! The process's IDs, its process group and session, and `kill`, which
//! go through the process manager's `Launcher`.

use crate::errno::{self, EINVAL, EIO, EPERM, ESRCH};
use core::mem::ManuallyDrop;
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{LauncherClient, ProcessInfo};
use mikros_rt::sys::{self, Handle};

const FOREVER: i64 = i64::MAX;

static PID: AtomicU32 = AtomicU32::new(0);
static PARENT: AtomicU32 = AtomicU32::new(0);
/// The `Launcher` channel of the startup transfer. Calls may share it,
/// since each reply goes to the call of its transaction.
static LAUNCHER: AtomicU32 = AtomicU32::new(0);

/// Keeps the IDs and the `Launcher` channel of the startup transfer.
pub fn install(pid: u32, parent: u32, launcher: Handle) {
    PID.store(pid, Ordering::Relaxed);
    PARENT.store(parent, Ordering::Relaxed);
    LAUNCHER.store(launcher, Ordering::Relaxed);
}

pub fn getpid() -> i64 {
    i64::from(PID.load(Ordering::Relaxed))
}

pub fn getppid() -> i64 {
    i64::from(PARENT.load(Ordering::Relaxed))
}

/// A client of the `Launcher` channel.
pub fn launcher() -> ManuallyDrop<LauncherClient> {
    // SAFETY: the channel stays open for as long as the process runs; the
    // client is never dropped, so it never closes it.
    ManuallyDrop::new(LauncherClient::new(ClientEnd::new(unsafe {
        OwnedHandle::from_raw(LAUNCHER.load(Ordering::Relaxed))
    })))
}

/// Runs `call` with every signal blocked, so that a signal does not
/// interrupt a call whose reply the caller needs; a signal sent to the
/// caller meanwhile is delivered as the mask is restored.
pub fn unsignalled<R>(call: impl FnOnce() -> R) -> R {
    let mut mask = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::signal_set_mask(0, u64::MAX, &mut mask) };
    let result = call();
    // SAFETY: the old mask may be null.
    unsafe { sys::signal_set_mask(2, mask, core::ptr::null_mut()) };
    result
}

/// `mikros_executable_path`: the executable path that the process
/// manager keeps for the calling process. Copies at most `len` bytes of
/// it to `buf`, without a terminating NUL, and returns its whole length,
/// or the negated error number when the process manager does not give
/// it.
///
/// # Safety
///
/// `buf` must have room for `len` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn mikros_executable_path(buf: *mut u8, len: usize) -> i64 {
    let pid = PID.load(Ordering::Relaxed);
    let info = match unsignalled(|| launcher().get_info(pid, FOREVER)) {
        Ok(Ok(reply)) => reply.info,
        Ok(Err(status)) => return -errno::from_status(i64::from(status.into_raw())),
        Err(_) => return -EIO,
    };
    let path = info.path.as_bytes();
    let count = path.len().min(len);
    // SAFETY: the caller gives room for `len` bytes, and `count` is at
    // most that.
    unsafe { core::ptr::copy_nonoverlapping(path.as_ptr(), buf, count) };
    path.len() as i64
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

/// What the process manager knows of process `pid`, 0 for the caller.
fn info(pid: i64) -> Result<ProcessInfo, i64> {
    let pid = match pid {
        0 => PID.load(Ordering::Relaxed),
        pid => u32::try_from(pid).map_err(|_| ESRCH)?,
    };
    match unsignalled(|| launcher().get_info(pid, FOREVER)) {
        Ok(Ok(reply)) => Ok(reply.info),
        Ok(Err(status)) => Err(kill_error(status)),
        Err(_) => Err(EIO),
    }
}

/// `getpgid`: the process group of process `pid`, 0 for the caller.
pub fn getpgid(pid: i64) -> i64 {
    info(pid).map_or_else(|e| -e, |i| i64::from(i.process_group))
}

/// `getsid`: the session of process `pid`, 0 for the caller.
pub fn getsid(pid: i64) -> i64 {
    info(pid).map_or_else(|e| -e, |i| i64::from(i.session))
}

/// `setsid`: makes the caller the leader of a new session and process
/// group, and returns its ID. Fails with `EPERM` when the caller leads a
/// process group.
pub fn setsid() -> i64 {
    match unsignalled(|| launcher().create_session(FOREVER)) {
        Ok(Ok(reply)) => i64::from(reply.session),
        Ok(Err(Status::BadState)) => -EPERM,
        Ok(Err(status)) => -kill_error(status),
        Err(_) => -EIO,
    }
}

/// `setpgid`: moves process `pid`, 0 for the caller, to process group
/// `pgid`, 0 for a group with the process's own ID. A process that is
/// not the caller or a child of its session, or a group of another
/// session or that does not exist, is refused with `EPERM`.
pub fn setpgid(pid: i64, pgid: i64) -> i64 {
    if pgid < 0 {
        return -EINVAL;
    }
    let pid = match pid {
        0 => PID.load(Ordering::Relaxed),
        pid => match u32::try_from(pid) {
            Ok(pid) => pid,
            Err(_) => return -ESRCH,
        },
    };
    let Ok(group) = u32::try_from(if pgid == 0 { i64::from(pid) } else { pgid }) else {
        return -EINVAL;
    };
    match unsignalled(|| launcher().set_process_group(pid, group, FOREVER)) {
        Ok(Ok(_)) => 0,
        Ok(Err(status)) => -kill_error(status),
        Err(_) => -EIO,
    }
}

/// `kill(-1, signo)`: sends `signo` through the process manager to every
/// process that its `List` gives the caller, but init and the caller,
/// leaving out those it may not signal and those that ended meanwhile.
/// Fails with `EPERM` when it may signal none of them, and with `ESRCH`
/// when there is none.
fn kill_all(signo: i64) -> i64 {
    let me = PID.load(Ordering::Relaxed);
    let client = launcher();
    let Ok(listed) = unsignalled(|| client.list(FOREVER)) else {
        return -EIO;
    };
    let (mut signalled, mut refused) = (false, false);
    for pid in listed.pids.into_iter().filter(|&p| p != 1 && p != me) {
        match unsignalled(|| client.kill(pid, signo as u32, FOREVER)) {
            Ok(Ok(_)) => signalled = true,
            Ok(Err(Status::AccessDenied)) => refused = true,
            Ok(Err(Status::NotFound)) => {}
            Ok(Err(status)) => return -kill_error(status),
            Err(_) => return -EIO,
        }
    }
    match (signalled, refused) {
        (true, _) => 0,
        (false, true) => -EPERM,
        (false, false) => -ESRCH,
    }
}

/// `kill`: sends `signo` to process `pid`, to the caller's process group
/// for 0, to every process it may signal for -1, or to process group
/// `-pid` below -1.
pub fn kill(pid: i64, signo: i64) -> i64 {
    if !(0..=64).contains(&signo) {
        return -EINVAL;
    }
    let group = match pid {
        1.. => None,
        0 => match info(0) {
            Ok(own) => Some(own.process_group),
            Err(e) => return -e,
        },
        ..=-2 => match u32::try_from(-pid) {
            Ok(group) => Some(group),
            Err(_) => return -ESRCH,
        },
        -1 => return kill_all(signo),
    };
    let client = launcher();
    let signo = signo as u32;
    let result = unsignalled(|| match group {
        None => client.kill(pid as u32, signo, FOREVER).map(|r| r.map(drop)),
        Some(group) => client
            .kill_group(group, signo, FOREVER)
            .map(|r| r.map(drop)),
    });
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(status)) => -kill_error(status),
        Err(_) => -EIO,
    }
}
