//! The C library's process, signal, terminal and time functions that
//! msh calls beyond what std gives.

use std::ffi::{CString, c_char, c_int, c_long};
use std::io;

pub const SIGKILL: i32 = 9;
pub const SIGCONT: i32 = 18;
const SIGINT: c_int = 2;
const SIGQUIT: c_int = 3;
const SIGTSTP: c_int = 20;
const SIGTTIN: c_int = 21;
const SIGTTOU: c_int = 22;
const WNOHANG: c_int = 1;
const WUNTRACED: c_int = 2;
const X_OK: c_int = 1;
const SIG_BLOCK: c_int = 0;
const SIG_SETMASK: c_int = 2;

/// musl's `struct tm`.
#[repr(C)]
struct Tm {
    fields: [c_int; 9],
    gmtoff: c_long,
    zone: *const c_char,
}

unsafe extern "C" {
    fn getpgrp() -> c_int;
    fn setpgid(pid: c_int, pgid: c_int) -> c_int;
    fn tcsetpgrp(fd: c_int, pgrp: c_int) -> c_int;
    fn tcgetpgrp(fd: c_int) -> c_int;
    fn isatty(fd: c_int) -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
    fn signal(sig: c_int, handler: extern "C" fn(c_int)) -> usize;
    fn sigprocmask(how: c_int, set: *const u64, old: *mut u64) -> c_int;
    fn access(path: *const c_char, mode: c_int) -> c_int;
    fn gethostname(name: *mut c_char, len: usize) -> c_int;
    fn time(t: *mut i64) -> i64;
    fn localtime_r(t: *const i64, tm: *mut Tm) -> *mut Tm;
}

/// How a waited-for process changed.
#[derive(Clone, Copy, Debug)]
pub enum Change {
    Exited(i32),
    Signaled(i32),
    Stopped(i32),
}

fn result(returned: c_int) -> io::Result<c_int> {
    match returned {
        -1 => Err(io::Error::last_os_error()),
        n => Ok(n),
    }
}

pub fn is_terminal(fd: i32) -> bool {
    // SAFETY: the call takes no pointers.
    unsafe { isatty(fd) == 1 }
}

/// Puts process `pid` into process group `group`, 0 for its own.
pub fn set_group(pid: i32, group: i32) -> io::Result<()> {
    // SAFETY: the call takes no pointers.
    result(unsafe { setpgid(pid, group) }).map(drop)
}

/// The shell's process group.
pub fn group() -> i32 {
    // SAFETY: the call takes no pointers.
    unsafe { getpgrp() }
}

/// The foreground process group of standard input's terminal, when it
/// is the shell's controlling terminal.
pub fn foreground() -> Option<i32> {
    // SAFETY: the call takes no pointers.
    let group = unsafe { tcgetpgrp(0) };
    (group > 0).then_some(group)
}

/// Makes `group` the foreground process group of standard input's
/// terminal, with `SIGTTOU` blocked, which a shell in the background
/// would otherwise get for it.
pub fn set_foreground(group: i32) -> io::Result<()> {
    // musl's `sigset_t`, whose first word holds the first 64 signals.
    let mut block = [0u64; 16];
    block[0] = 1 << (SIGTTOU - 1);
    let mut was = [0u64; 16];
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        sigprocmask(SIG_BLOCK, block.as_ptr(), was.as_mut_ptr());
        let done = result(tcsetpgrp(0, group)).map(drop);
        sigprocmask(SIG_SETMASK, was.as_ptr(), std::ptr::null_mut());
        done
    }
}

/// Waits for process `pid`, or any child for -1, to end or stop, and
/// gives its ID and how it changed, or `None` when `block` is false and
/// none did.
pub fn wait(pid: i32, block: bool) -> io::Result<Option<(i32, Change)>> {
    let mut status: c_int = 0;
    let options = WUNTRACED | if block { 0 } else { WNOHANG };
    // SAFETY: the pointer is valid for the call.
    let got = result(unsafe { waitpid(pid, &mut status, options) })?;
    if got == 0 {
        return Ok(None);
    }
    let change = if status & 0x7f == 0 {
        Change::Exited((status >> 8) & 0xff)
    } else if status & 0xff == 0x7f {
        Change::Stopped((status >> 8) & 0xff)
    } else {
        Change::Signaled(status & 0x7f)
    };
    Ok(Some((got, change)))
}

/// Sends `sig` to process `pid`, or to process group `-pid`.
pub fn send(pid: i32, sig: i32) -> io::Result<()> {
    // SAFETY: the call takes no pointers.
    result(unsafe { kill(pid, sig) }).map(drop)
}

/// Whether the file at `path` may be run.
pub fn may_run(path: &str) -> bool {
    let Ok(path) = CString::new(path) else {
        return false;
    };
    // SAFETY: the path is NUL-terminated.
    unsafe { access(path.as_ptr(), X_OK) == 0 }
}

/// The host's name, as the C library gives it, or nothing when it
/// gives none.
pub fn host_name() -> String {
    let mut name = [0u8; 256];
    // SAFETY: the buffer is valid for its length.
    if unsafe { gethostname(name.as_mut_ptr().cast(), name.len()) } != 0 {
        return String::new();
    }
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    String::from_utf8_lossy(&name[..end]).into_owned()
}

/// The local time, in seconds since 1970-01-01T00:00:00 local.
pub fn local_time() -> i64 {
    let mut tm = Tm {
        fields: [0; 9],
        gmtoff: 0,
        zone: std::ptr::null(),
    };
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        let now = time(std::ptr::null_mut());
        match localtime_r(&now, &mut tm).is_null() {
            true => now,
            false => now + tm.gmtoff,
        }
    }
}

extern "C" fn ignore(_: c_int) {}

/// Keeps the signals of the terminal's keys and of background access
/// from stopping or ending an interactive shell. A handler, unlike
/// ignoring them, does not pass to the programs it starts.
pub fn shield() {
    for sig in [SIGINT, SIGQUIT, SIGTSTP, SIGTTIN, SIGTTOU] {
        // SAFETY: `ignore` is a valid handler for the whole program.
        unsafe { signal(sig, ignore) };
    }
}
