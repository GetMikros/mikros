//! `__mikros_syscall`, which musl's system call layer calls with Linux
//! system call numbers and arguments. Each operation is performed with
//! Mikros system calls, and the result is returned as musl expects: a
//! value, or a negated error number. Operations that the C library does
//! not perform yet fail with `ENOSYS`.

use crate::errno::{self, EACCES, EBADF, EINVAL, ENOSYS, ENOTTY, EPIPE};
use crate::fd;
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::sys::{self, Handle};

const SYS_WRITE: i64 = 1;
const SYS_MMAP: i64 = 9;
const SYS_MPROTECT: i64 = 10;
const SYS_MUNMAP: i64 = 11;
const SYS_BRK: i64 = 12;
const SYS_IOCTL: i64 = 16;
const SYS_WRITEV: i64 = 20;
const SYS_EXIT: i64 = 60;
const SYS_SET_THREAD_AREA: i64 = 205;
const SYS_SET_TID_ADDRESS: i64 = 218;
const SYS_EXIT_GROUP: i64 = 231;
const SYS_GETRANDOM: i64 = 318;

const PROT_READ: i64 = 1;
const PROT_WRITE: i64 = 2;
const PROT_EXEC: i64 = 4;
const MAP_FIXED: i64 = 0x10;
const MAP_ANONYMOUS: i64 = 0x20;

const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const PERM_EXECUTE: u32 = 1 << 2;
const SPECIFIC: u32 = 1 << 8;
const PAGE_SIZE: u64 = 4096;

const WRITABLE: u32 = 1 << 1;
const PEER_CLOSED: u32 = 1 << 2;
const SHOULD_WAIT: sys::Status = -22;
const STATUS_PEER_CLOSED: sys::Status = -24;

/// The process handle, for the mappings of `mmap`.
static PROCESS: AtomicU32 = AtomicU32::new(0);
/// The object ID of the first thread, which `set_tid_address` returns.
static TID: AtomicU32 = AtomicU32::new(0);

/// Records the handles that the system calls work on.
pub fn set_process(process: Handle, tid: u32) {
    PROCESS.store(process, Ordering::Relaxed);
    TID.store(tid, Ordering::Relaxed);
}

#[unsafe(no_mangle)]
extern "C" fn __mikros_syscall(
    n: i64,
    a1: i64,
    a2: i64,
    a3: i64,
    a4: i64,
    a5: i64,
    a6: i64,
) -> i64 {
    let _ = a6;
    match n {
        SYS_WRITE => write(a1, a2 as *const u8, a3 as u64),
        SYS_WRITEV => writev(a1, a2 as *const Iovec, a3),
        SYS_IOCTL => ioctl(a1),
        SYS_MMAP => mmap(a1 as u64, a2 as u64, a3, a4, a5),
        SYS_MPROTECT => mprotect(a1 as u64, a2 as u64, a3),
        // SAFETY: unmapping takes no pointers.
        SYS_MUNMAP => errno::result(unsafe { sys::vm_unmap(process(), a1 as u64, a2 as u64) }, 0),
        // There is no program break; malloc maps memory instead.
        SYS_BRK => 0,
        SYS_SET_THREAD_AREA => {
            // SAFETY: setting the FS base takes no pointers.
            errno::result(unsafe { sys::thread_set_user_base(0, a1 as u64) }, 0)
        }
        SYS_SET_TID_ADDRESS => i64::from(TID.load(Ordering::Relaxed)),
        SYS_GETRANDOM => getrandom(a1 as *mut u8, a2 as u64),
        SYS_EXIT_GROUP => exit(a1),
        SYS_EXIT => {
            // SAFETY: ending the thread takes no pointers.
            unsafe { sys::thread_exit() };
            exit(a1)
        }
        _ => -ENOSYS,
    }
}

fn process() -> Handle {
    PROCESS.load(Ordering::Relaxed)
}

fn exit(code: i64) -> ! {
    // SAFETY: ending the process takes no pointers.
    unsafe { sys::process_exit(code) };
    unreachable!("process_exit returned");
}

#[repr(C)]
struct Iovec {
    base: *const u8,
    len: u64,
}

/// Writes `len` bytes at `buf` to descriptor `fd`, waiting while the
/// stream is full.
fn write(fd: i64, buf: *const u8, len: u64) -> i64 {
    let Some(stream) = fd::stream(fd) else {
        return -EBADF;
    };
    loop {
        let mut actual = 0;
        // SAFETY: musl passes a buffer of `len` bytes.
        let status = unsafe { sys::stream_write(stream, buf, len, &mut actual) };
        if status != SHOULD_WAIT {
            return match status {
                STATUS_PEER_CLOSED => -EPIPE,
                _ => errno::result(status, actual as i64),
            };
        }
        let mut observed = 0;
        // SAFETY: the out pointer is valid for the call.
        let waited = unsafe {
            sys::object_wait_one(stream, WRITABLE | PEER_CLOSED, i64::MAX, &mut observed)
        };
        if waited != mikros_rt::OK {
            return -errno::from_status(waited);
        }
    }
}

/// Writes the `count` buffers at `iov` in order, and stops at the first
/// that is written only in part.
fn writev(fd: i64, iov: *const Iovec, count: i64) -> i64 {
    if !(0..=1024).contains(&count) {
        return -EINVAL;
    }
    let mut total = 0;
    for i in 0..count as usize {
        // SAFETY: musl passes `count` buffers.
        let v = unsafe { &*iov.add(i) };
        if v.len == 0 {
            continue;
        }
        let written = write(fd, v.base, v.len);
        if written < 0 {
            return if total > 0 { total } else { written };
        }
        total += written;
        if (written as u64) < v.len {
            break;
        }
    }
    total
}

/// No descriptor is a terminal yet.
fn ioctl(fd: i64) -> i64 {
    if fd::stream(fd).is_some() {
        -ENOTTY
    } else {
        -EBADF
    }
}

/// The Mikros permissions of `prot`, or `EACCES` when it asks for
/// writable and executable memory.
fn perms(prot: i64) -> Result<u32, i64> {
    if prot & PROT_WRITE != 0 && prot & PROT_EXEC != 0 {
        return Err(EACCES);
    }
    let mut perms = 0;
    if prot & (PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        perms |= PERM_READ;
    }
    if prot & PROT_WRITE != 0 {
        perms |= PERM_WRITE;
    }
    if prot & PROT_EXEC != 0 {
        perms |= PERM_EXECUTE;
    }
    Ok(perms)
}

/// Maps a new anonymous memory object of `len` bytes. A fixed mapping
/// replaces whatever was mapped in its range. Files cannot be mapped
/// yet.
fn mmap(addr: u64, len: u64, prot: i64, flags: i64, fd: i64) -> i64 {
    let _ = fd;
    if flags & MAP_ANONYMOUS == 0 {
        return -ENOSYS;
    }
    if len == 0 {
        return -EINVAL;
    }
    let mut options = match perms(prot) {
        Ok(perms) => perms,
        Err(e) => return -e,
    };
    let Some(size) = len.checked_next_multiple_of(PAGE_SIZE) else {
        return -errno::ENOMEM;
    };
    let fixed = flags & MAP_FIXED != 0;
    if fixed {
        options |= SPECIFIC;
        // SAFETY: unmapping takes no pointers. Nothing may be mapped
        // there, which is no error.
        unsafe { sys::vm_unmap(process(), addr, size) };
    }
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::mem_create(size, 0, &mut mem) };
    if status != mikros_rt::OK {
        return -errno::from_status(status);
    }
    let mut mapped = 0;
    let at = if fixed { addr } else { 0 };
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::vm_map(process(), options, at, mem, 0, size, &mut mapped) };
    // The mapping keeps the memory object alive.
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(mem) };
    errno::result(status, mapped as i64)
}

fn mprotect(addr: u64, len: u64, prot: i64) -> i64 {
    match perms(prot) {
        // SAFETY: changing permissions takes no pointers.
        Ok(perms) => errno::result(unsafe { sys::vm_protect(process(), perms, addr, len) }, 0),
        Err(e) => -e,
    }
}

fn getrandom(buf: *mut u8, len: u64) -> i64 {
    // SAFETY: musl passes a buffer of `len` bytes.
    errno::result(unsafe { sys::system_get_random(buf, len) }, len as i64)
}
