//! Starting programs with the process manager's `Spawn`, replacing the
//! process with `Exec`, and waiting for children with `Wait`.
//!
//! `posix_spawn` builds the new program's descriptors from the caller's
//! that do not close on `exec`, changed by the file actions in order, and
//! passes a duplicate of each one's handle: descriptors 0 to 2 with the
//! standard roles and the rest with the role `USER`, each with an `id`
//! that holds its number and kind. A new program starts in `/`, with
//! every signal unblocked and at its default action, so the file actions
//! that change directory, and the attributes that ask for a mask, a
//! process group, a session or a scheduler, are not supported.

use crate::errno::{self, E2BIG, EBADF, ECHILD, EINTR, EINVAL, EIO, ENOENT, ENOSYS, ENOTDIR};
use crate::fd::{self, Description};
use crate::lock::Lock;
use crate::{clock, fs, process};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::{CStr, c_char};
use mikros_rt::interfaces::__midl::{self, OwnedHandle};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    HandleRole, SpawnRequest, StartupHandle, StatusKind, WaitOptions,
};
use mikros_rt::sys;

const FDOP_CLOSE: i32 = 1;
const FDOP_DUP2: i32 = 2;
const FDOP_OPEN: i32 = 3;
const FDOP_CHDIR: i32 = 4;
const FDOP_FCHDIR: i32 = 5;
const POSIX_SPAWN_RESETIDS: i32 = 1;
const POSIX_SPAWN_SETSIGDEF: i32 = 4;
const POSIX_SPAWN_SETSIGMASK: i32 = 8;
const POSIX_SPAWN_USEVFORK: i32 = 64;
/// The most handles a `SpawnRequest` carries.
const HANDLES_MAX: usize = 32;
/// Rights that a duplicate keeps: those of the original.
const SAME_RIGHTS: u32 = 1 << 31;
/// The user ID that names the caller's.
const CALLER_USER: u32 = u32::MAX;
const WNOHANG: i64 = 1;
const WUNTRACED: i64 = 2;
const WCONTINUED: i64 = 8;
const INTERRUPTED: i32 = -27;
const FOREVER: i64 = i64::MAX;
const PAGE_SIZE: u64 = 4096;

/// musl's `struct fdop`, a file action.
#[repr(C)]
pub struct Fdop {
    next: *const Fdop,
    prev: *const Fdop,
    cmd: i32,
    fd: i32,
    srcfd: i32,
    oflag: i32,
    mode: u32,
    path: [c_char; 0],
}

/// musl's `posix_spawnattr_t`.
#[repr(C)]
pub struct SpawnAttr {
    flags: i32,
    pgrp: i32,
    default: [u64; 16],
    mask: [u64; 16],
}

/// The descriptors of a new program: description and close-on-exec.
type Descriptors = BTreeMap<usize, (Arc<Description>, bool)>;

/// The strings of a null-terminated array of C strings.
///
/// # Safety
///
/// `array` must be null or a null-terminated array of C strings.
unsafe fn strings<'a>(array: *const *const c_char) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    if array.is_null() {
        return out;
    }
    let mut i = 0;
    // SAFETY: the caller passes a null-terminated array.
    while let Some(s) = unsafe { (*array.add(i)).as_ref() } {
        // SAFETY: each entry is a C string.
        out.push(unsafe { CStr::from_ptr(s) }.to_bytes());
        i += 1;
    }
    out
}

/// A memory object with the argument count, the environment count and
/// the strings, each ending in NUL, that a `SpawnRequest` carries.
fn arguments(args: &[&[u8]], env: &[&[u8]]) -> Result<OwnedHandle, i64> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(args.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(env.len() as u32).to_le_bytes());
    for s in args.iter().chain(env) {
        bytes.extend_from_slice(s);
        bytes.push(0);
    }
    let mut mem = 0;
    // Memory objects hold whole pages.
    let size = (bytes.len() as u64).next_multiple_of(PAGE_SIZE);
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::mem_create(size, 0, &mut mem) };
    if status != mikros_rt::OK {
        return Err(errno::from_status(status));
    }
    // SAFETY: the kernel just created the handle for us.
    let mem = unsafe { OwnedHandle::from_raw(mem) };
    // SAFETY: the buffer holds `bytes.len()` bytes.
    let status = unsafe { sys::mem_write(mem.raw(), 0, bytes.as_ptr(), bytes.len() as u64) };
    match status {
        mikros_rt::OK => Ok(mem),
        status => Err(errno::from_status(status)),
    }
}

/// Applies the file actions from `actions`, the most recent, back to the
/// first, in the order they were added, and returns the working folder
/// they leave the child in: the caller's current one unless one of them
/// changes it. Paths that do not start with `/` are relative to it.
///
/// # Safety
///
/// `actions` must be null or the newest of musl's list of file actions.
unsafe fn apply(descriptors: &mut Descriptors, actions: *const Fdop) -> Result<String, i64> {
    let mut folder = fs::cwd();
    let mut op = actions;
    // SAFETY: the list is linked both ways and ends in null.
    unsafe {
        while !op.is_null() && !(*op).next.is_null() {
            op = (*op).next;
        }
        while let Some(action) = op.as_ref() {
            // Only the descriptor actions have a descriptor.
            let fd = || usize::try_from(action.fd).map_err(|_| EBADF);
            match action.cmd {
                FDOP_CLOSE => {
                    descriptors.remove(&fd()?);
                }
                FDOP_DUP2 => {
                    let src = usize::try_from(action.srcfd).map_err(|_| EBADF)?;
                    let (description, _) = descriptors.get(&src).cloned().ok_or(EBADF)?;
                    descriptors.insert(fd()?, (description, false));
                }
                FDOP_OPEN => {
                    let path = CStr::from_ptr(action.path.as_ptr()).to_bytes();
                    let mut at = fs::normalize(&folder, path).ok_or(ENOENT)?.into_bytes();
                    at.push(0);
                    let flags = action.oflag as u32;
                    let opened =
                        fs::open_description(fs::AT_FDCWD, at.as_ptr(), flags, action.mode)?;
                    descriptors.insert(fd()?, (opened, flags & fs::O_CLOEXEC != 0));
                }
                FDOP_CHDIR => {
                    let path = CStr::from_ptr(action.path.as_ptr()).to_bytes();
                    folder = fs::directory_at(&folder, path)?;
                }
                FDOP_FCHDIR => match descriptors.get(&fd()?).map(|(d, _)| &**d) {
                    Some(Description::Directory(node, _)) => folder = node.path.clone(),
                    Some(_) => return Err(ENOTDIR),
                    None => return Err(EBADF),
                },
                _ => return Err(ENOSYS),
            }
            op = action.prev;
        }
    }
    Ok(folder)
}

/// The startup handles for `descriptors` that do not close on `exec`.
fn handles(descriptors: &Descriptors) -> Result<Vec<StartupHandle>, i64> {
    let mut out = Vec::new();
    for (&fd, (description, cloexec)) in descriptors {
        // An epoll instance leaves the descriptor closed in the new
        // program.
        if *cloexec || !fd::passed(description) {
            continue;
        }
        if out.len() == HANDLES_MAX || fd > fd::ID_FD as usize {
            return Err(E2BIG);
        }
        let (raw, kind) = fd::shared_handle(description).ok_or(EBADF)?;
        let mut copy = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::handle_duplicate(raw, SAME_RIGHTS, &mut copy) };
        if status != mikros_rt::OK {
            return Err(errno::from_status(status));
        }
        let role = match fd {
            0 => HandleRole::Stdin,
            1 => HandleRole::Stdout,
            2 => HandleRole::Stderr,
            _ => HandleRole::User,
        };
        out.push(StartupHandle {
            role,
            id: fd as u32 | kind << 28,
            // SAFETY: the kernel just created the handle for us.
            handle: unsafe { OwnedHandle::from_raw(copy) },
        });
    }
    Ok(out)
}

/// `posix_spawn`: starts the program at `path` with `argv` and `envp`,
/// stores its process ID at `res`, and returns 0 or an error number.
///
/// # Safety
///
/// The pointers must be as `posix_spawn` takes them, with `actions` the
/// newest of musl's list of file actions.
#[unsafe(no_mangle)]
unsafe extern "C" fn __mikros_spawn(
    res: *mut i32,
    path: *const c_char,
    actions: *const Fdop,
    attr: *const SpawnAttr,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> i32 {
    // SAFETY: the caller passes what `posix_spawn` takes.
    match unsafe { spawn(path, actions, attr, argv, envp) } {
        Ok(pid) => {
            if !res.is_null() {
                // SAFETY: the caller passes room for the process ID.
                unsafe { res.write(pid as i32) };
            }
            0
        }
        Err(e) => e as i32,
    }
}

/// The caller's descriptors.
fn descriptors() -> Descriptors {
    fd::snapshot()
        .into_iter()
        .map(|(fd, description, cloexec)| (fd, (description, cloexec)))
        .collect()
}

/// The request that starts the program at `path` with `argv`, `envp` and
/// `descriptors`, in the working folder `folder`.
///
/// # Safety
///
/// `path` must be a C string, and `argv` and `envp` null or
/// null-terminated arrays of C strings.
unsafe fn request(
    path: *const c_char,
    descriptors: &Descriptors,
    folder: String,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> Result<SpawnRequest, i64> {
    // SAFETY: the caller passes a C string.
    let path = unsafe { fs::c_path(path.cast()) }?;
    let path: String = fs::absolute(fs::AT_FDCWD, path)?;
    let handles = handles(descriptors)?;
    // SAFETY: the caller passes null-terminated arrays of C strings.
    let (args, env) = unsafe { (strings(argv), strings(envp)) };
    Ok(SpawnRequest {
        path,
        arguments: arguments(&args, &env)?,
        handles,
        app_id: None,
        user: CALLER_USER,
        login_session: false,
        new_session: false,
        working_directory: Some(folder),
    })
}

/// `execve`: replaces the process with the program at `path`, given
/// `argv`, `envp` and the descriptors that do not close on `exec`. It
/// returns only on failure. The new program starts in the current
/// folder, with every signal unblocked and at its default action.
///
/// # Safety
///
/// The pointers must be as `execve` takes them.
pub unsafe fn execve(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> i64 {
    // SAFETY: the caller passes what `execve` takes.
    let request = match unsafe { request(path, &descriptors(), fs::cwd(), argv, envp) } {
        Ok(request) => request,
        Err(e) => return -e,
    };
    let client = process::launcher();
    match process::unsignalled(|| client.exec(request, FOREVER)) {
        // The process manager kills the process before it replies, and
        // no longer reports its end.
        Ok(Ok(_)) => {
            // SAFETY: the call takes no pointers.
            unsafe { sys::process_exit(0) };
            unreachable!("process_exit returned")
        }
        Ok(Err(status)) => -errno::from_status(i64::from(status.into_raw())),
        Err(_) => -EIO,
    }
}

/// # Safety
///
/// As for `__mikros_spawn`.
unsafe fn spawn(
    path: *const c_char,
    actions: *const Fdop,
    attr: *const SpawnAttr,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> Result<u32, i64> {
    // SAFETY: the caller passes attributes or null.
    if let Some(attr) = unsafe { attr.as_ref() } {
        let known = POSIX_SPAWN_RESETIDS | POSIX_SPAWN_SETSIGDEF | POSIX_SPAWN_USEVFORK;
        let mask_flag = attr.flags & POSIX_SPAWN_SETSIGMASK != 0;
        if attr.flags & !(known | POSIX_SPAWN_SETSIGMASK) != 0
            || mask_flag && attr.mask.iter().any(|&w| w != 0)
        {
            return Err(ENOSYS);
        }
    }
    let mut descriptors = descriptors();
    // SAFETY: the caller passes musl's list of file actions.
    let folder = unsafe { apply(&mut descriptors, actions) }?;
    // SAFETY: the caller passes what `posix_spawn` takes.
    let request = unsafe { request(path, &descriptors, folder, argv, envp) }?;
    let client = process::launcher();
    match process::unsignalled(|| client.spawn(request, FOREVER)) {
        Ok(Ok(spawned)) => {
            CHILDREN.lock().insert(spawned.pid, spawned.process);
            Ok(spawned.pid)
        }
        Ok(Err(status)) => Err(errno::from_status(i64::from(status.into_raw()))),
        Err(_) => Err(EIO),
    }
}

/// The process handles that `Spawn` gave for the children that have not
/// ended or been waited for, by process ID, until
/// `mikros_process_handle` takes them.
static CHILDREN: Lock<BTreeMap<u32, OwnedHandle>> = Lock::new(BTreeMap::new());

/// `mikros_process_handle`: takes the process handle of the child `pid`
/// that `posix_spawn` started, which the caller owns from then on, or
/// returns `-ECHILD` when there is none: when `pid` is not such a child,
/// was waited for, or its handle was taken already.
#[unsafe(no_mangle)]
extern "C" fn mikros_process_handle(pid: i32) -> i64 {
    let taken = u32::try_from(pid)
        .ok()
        .and_then(|pid| CHILDREN.lock().remove(&pid));
    match taken {
        Some(handle) => i64::from(handle.into_raw()),
        None => -ECHILD,
    }
}

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
    // A child that ended is gone, and so is its handle.
    if s.kind == StatusKind::Exited {
        CHILDREN.lock().remove(&ended.pid);
    }
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
