//! `__mikros_syscall`, which musl's system call layer calls with Linux
//! system call numbers and arguments. Each operation is performed with
//! Mikros system calls, and the result is returned as musl expects: a
//! value, or a negated error number. Operations that the C library does
//! not perform yet fail with `ENOSYS`.

use crate::errno::{self, EACCES, EBADF, EINVAL, ENOSYS, ENOTTY};
use crate::fd::Description;
use crate::{clock, dir, fd, fs, io, stat};
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::sys::{self, Handle};

const SYS_READ: i64 = 0;
const SYS_WRITE: i64 = 1;
const SYS_OPEN: i64 = 2;
const SYS_CLOSE: i64 = 3;
const SYS_STAT: i64 = 4;
const SYS_FSTAT: i64 = 5;
const SYS_LSTAT: i64 = 6;
const SYS_LSEEK: i64 = 8;
const SYS_MMAP: i64 = 9;
const SYS_MPROTECT: i64 = 10;
const SYS_MUNMAP: i64 = 11;
const SYS_BRK: i64 = 12;
const SYS_IOCTL: i64 = 16;
const SYS_PREAD64: i64 = 17;
const SYS_PWRITE64: i64 = 18;
const SYS_READV: i64 = 19;
const SYS_WRITEV: i64 = 20;
const SYS_DUP: i64 = 32;
const SYS_DUP2: i64 = 33;
const SYS_EXIT: i64 = 60;
const SYS_FCNTL: i64 = 72;
const SYS_GETCWD: i64 = 79;
const SYS_CHDIR: i64 = 80;
const SYS_FCHDIR: i64 = 81;
const SYS_READLINK: i64 = 89;
const SYS_SET_THREAD_AREA: i64 = 205;
const SYS_GETDENTS64: i64 = 217;
const SYS_SET_TID_ADDRESS: i64 = 218;
const SYS_CLOCK_GETTIME: i64 = 228;
const SYS_EXIT_GROUP: i64 = 231;
const SYS_OPENAT: i64 = 257;
const SYS_NEWFSTATAT: i64 = 262;
const SYS_READLINKAT: i64 = 267;
const SYS_DUP3: i64 = 292;
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

const F_DUPFD: i64 = 0;
const F_GETFD: i64 = 1;
const F_SETFD: i64 = 2;
const F_GETFL: i64 = 3;
const F_SETFL: i64 = 4;
const F_DUPFD_CLOEXEC: i64 = 1030;
const FD_CLOEXEC: i64 = 1;

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
        SYS_READ => io::read(a1, a2 as *mut u8, a3 as u64),
        SYS_WRITE => io::write(a1, a2 as *const u8, a3 as u64),
        SYS_READV => io::vectored(a1, a2 as *const io::Iovec, a3, false),
        SYS_WRITEV => io::vectored(a1, a2 as *const io::Iovec, a3, true),
        SYS_PREAD64 => io::pread(a1, a2 as *mut u8, a3 as u64, a4),
        SYS_PWRITE64 => io::pwrite(a1, a2 as *const u8, a3 as u64, a4),
        SYS_LSEEK => io::lseek(a1, a2, a3),
        SYS_OPEN => fs::open(fs::AT_FDCWD, a1 as *const u8, a2 as u32, a3 as u32),
        SYS_OPENAT => fs::open(a1, a2 as *const u8, a3 as u32, a4 as u32),
        SYS_CLOSE => fd::close(a1),
        SYS_STAT => stat::fstatat(fs::AT_FDCWD, a1 as *const u8, a2 as *mut stat::Kstat, 0),
        SYS_LSTAT => {
            let out = a2 as *mut stat::Kstat;
            stat::fstatat(
                fs::AT_FDCWD,
                a1 as *const u8,
                out,
                stat::AT_SYMLINK_NOFOLLOW,
            )
        }
        SYS_FSTAT => stat::fstat(a1, a2 as *mut stat::Kstat),
        SYS_NEWFSTATAT => stat::fstatat(a1, a2 as *const u8, a3 as *mut stat::Kstat, a4),
        SYS_GETDENTS64 => dir::getdents(a1, a2 as *mut u8, a3 as u64),
        SYS_GETCWD => fs::getcwd(a1 as *mut u8, a2 as u64),
        SYS_CHDIR => fs::chdir(a1 as *const u8),
        SYS_FCHDIR => fs::fchdir(a1),
        SYS_READLINK => fs::readlink(fs::AT_FDCWD, a1 as *const u8, a2 as *mut u8, a3 as u64),
        SYS_READLINKAT => fs::readlink(a1, a2 as *const u8, a3 as *mut u8, a4 as u64),
        SYS_DUP => fd::duplicate_from(a1, 0, false),
        SYS_DUP2 => dup2(a1, a2),
        SYS_DUP3 => dup3(a1, a2, a3 as u32),
        SYS_FCNTL => fcntl(a1, a2, a3),
        // No descriptor is a terminal yet.
        SYS_IOCTL => {
            if fd::get(a1).is_some() {
                -ENOTTY
            } else {
                -EBADF
            }
        }
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
        SYS_CLOCK_GETTIME => clock::gettime(a1, a2 as *mut clock::Timespec),
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

fn dup2(from: i64, to: i64) -> i64 {
    if from == to {
        return if fd::get(from).is_some() { to } else { -EBADF };
    }
    fd::duplicate_to(from, to, false)
}

fn dup3(from: i64, to: i64, flags: u32) -> i64 {
    if from == to || flags & !fs::O_CLOEXEC != 0 {
        return -EINVAL;
    }
    fd::duplicate_to(from, to, flags & fs::O_CLOEXEC != 0)
}

/// The status flags that `F_SETFL` changes.
const SETFL_MASK: u32 = fs::O_APPEND | fs::O_NONBLOCK;

fn fcntl(fd: i64, cmd: i64, arg: i64) -> i64 {
    match cmd {
        F_DUPFD => fd::duplicate_from(fd, arg, false),
        F_DUPFD_CLOEXEC => fd::duplicate_from(fd, arg, true),
        F_GETFD => match fd::close_on_exec(fd) {
            Some(cloexec) => i64::from(cloexec) * FD_CLOEXEC,
            None => -EBADF,
        },
        F_SETFD => fd::set_close_on_exec(fd, arg & FD_CLOEXEC != 0),
        F_GETFL | F_SETFL => {
            let Some(description) = fd::get(fd) else {
                return -EBADF;
            };
            let node = match &*description {
                Description::File(node) | Description::Directory(node, _) => node,
                // Streams are read and written, and have no other flags.
                Description::Stream(_) => {
                    return if cmd == F_GETFL {
                        i64::from(fs::O_RDWR)
                    } else {
                        0
                    };
                }
            };
            let flags = &node.flags;
            if cmd == F_GETFL {
                return i64::from(flags.load(Ordering::Relaxed));
            }
            let old = flags.load(Ordering::Relaxed);
            flags.store(
                old & !SETFL_MASK | arg as u32 & SETFL_MASK,
                Ordering::Relaxed,
            );
            0
        }
        _ => -EINVAL,
    }
}
