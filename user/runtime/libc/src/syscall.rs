//! `__mikros_syscall`, which musl's system call layer calls with Linux
//! system call numbers and arguments. Each operation is performed with
//! Mikros system calls, and the result is returned as musl expects: a
//! value, or a negated error number. Operations that the C library does
//! not perform yet fail with `ENOSYS`.

use crate::errno::{self, EACCES, EBADF, EINVAL, ENODEV, ENOSYS, ENOTTY};
use crate::fd::Description;
use crate::{
    clock, dir, epoll, fd, fs, futex, io, poll, process, signal, spawn, stat, thread, timerfd,
};
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::fs::MemoryFlags;
use mikros_rt::sys::{self, Handle};

const SYS_READ: i64 = 0;
const SYS_WRITE: i64 = 1;
const SYS_OPEN: i64 = 2;
const SYS_CLOSE: i64 = 3;
const SYS_STAT: i64 = 4;
const SYS_FSTAT: i64 = 5;
const SYS_LSTAT: i64 = 6;
const SYS_POLL: i64 = 7;
const SYS_SELECT: i64 = 23;
const SYS_LSEEK: i64 = 8;
const SYS_MMAP: i64 = 9;
const SYS_MPROTECT: i64 = 10;
const SYS_MUNMAP: i64 = 11;
const SYS_BRK: i64 = 12;
const SYS_RT_SIGACTION: i64 = 13;
const SYS_RT_SIGPROCMASK: i64 = 14;
const SYS_IOCTL: i64 = 16;
const SYS_PREAD64: i64 = 17;
const SYS_PWRITE64: i64 = 18;
const SYS_READV: i64 = 19;
const SYS_WRITEV: i64 = 20;
const SYS_ACCESS: i64 = 21;
const SYS_PIPE: i64 = 22;
const SYS_DUP: i64 = 32;
const SYS_GETPID: i64 = 39;
const SYS_DUP2: i64 = 33;
const SYS_NANOSLEEP: i64 = 35;
const SYS_EXECVE: i64 = 59;
const SYS_EXIT: i64 = 60;
const SYS_WAIT4: i64 = 61;
const SYS_KILL: i64 = 62;
const SYS_FCNTL: i64 = 72;
const SYS_GETCWD: i64 = 79;
const SYS_CHDIR: i64 = 80;
const SYS_FCHDIR: i64 = 81;
const SYS_READLINK: i64 = 89;
const SYS_SETPGID: i64 = 109;
const SYS_GETPPID: i64 = 110;
const SYS_SETSID: i64 = 112;
const SYS_GETPGID: i64 = 121;
const SYS_GETSID: i64 = 124;
const SYS_PAUSE: i64 = 34;
const SYS_RT_SIGPENDING: i64 = 127;
const SYS_RT_SIGTIMEDWAIT: i64 = 128;
const SYS_RT_SIGSUSPEND: i64 = 130;
const SYS_SIGALTSTACK: i64 = 131;
const SYS_GETTID: i64 = 186;
const SYS_TKILL: i64 = 200;
const SYS_FUTEX: i64 = 202;
const SYS_SET_THREAD_AREA: i64 = 205;
const SYS_GETDENTS64: i64 = 217;
const SYS_SET_TID_ADDRESS: i64 = 218;
const SYS_CLOCK_GETTIME: i64 = 228;
const SYS_CLOCK_NANOSLEEP: i64 = 230;
const SYS_EXIT_GROUP: i64 = 231;
const SYS_TGKILL: i64 = 234;
const SYS_OPENAT: i64 = 257;
const SYS_NEWFSTATAT: i64 = 262;
const SYS_PSELECT6: i64 = 270;
const SYS_PPOLL: i64 = 271;
const SYS_EPOLL_CREATE: i64 = 213;
const SYS_EPOLL_WAIT: i64 = 232;
const SYS_EPOLL_CTL: i64 = 233;
const SYS_EPOLL_PWAIT: i64 = 281;
const SYS_EPOLL_CREATE1: i64 = 291;
const SYS_TIMERFD_CREATE: i64 = 283;
const SYS_TIMERFD_SETTIME: i64 = 286;
const SYS_TIMERFD_GETTIME: i64 = 287;
const SYS_READLINKAT: i64 = 267;
const SYS_FACCESSAT: i64 = 269;
const SYS_DUP3: i64 = 292;
const SYS_PIPE2: i64 = 293;
const SYS_GETRANDOM: i64 = 318;
const SYS_FACCESSAT2: i64 = 439;

const PROT_READ: i64 = 1;
const PROT_WRITE: i64 = 2;
const PROT_EXEC: i64 = 4;
const MAP_SHARED: i64 = 0x01;
const MAP_PRIVATE: i64 = 0x02;
const MAP_SHARED_VALIDATE: i64 = 0x03;
const MAP_TYPE: i64 = 0x0f;
const MAP_FIXED: i64 = 0x10;
const MAP_ANONYMOUS: i64 = 0x20;

const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const PERM_EXECUTE: u32 = 1 << 2;
const SPECIFIC: u32 = 1 << 8;
const PAGE_SIZE: u64 = 4096;
/// The `object_get_info` topic of a memory object's size and kind.
const INFO_MEMORY: u32 = 4;
const INFO_MEMORY_SIZE: usize = 24;
const FOREVER: i64 = i64::MAX;

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
        SYS_ACCESS => fs::access(fs::AT_FDCWD, a1 as *const u8, a2, 0),
        SYS_FACCESSAT => fs::access(a1, a2 as *const u8, a3, 0),
        SYS_FACCESSAT2 => fs::access(a1, a2 as *const u8, a3, a4),
        SYS_PIPE => fd::pipe(a1 as *mut [i32; 2], 0),
        SYS_PIPE2 => fd::pipe(a1 as *mut [i32; 2], a2 as u32),
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
        SYS_MMAP => mmap(a1 as u64, a2 as u64, a3, a4, a5, a6),
        SYS_MPROTECT => mprotect(a1 as u64, a2 as u64, a3),
        // SAFETY: unmapping takes no pointers.
        SYS_MUNMAP => errno::result(unsafe { sys::vm_unmap(process(), a1 as u64, a2 as u64) }, 0),
        // There is no program break; malloc maps memory instead.
        SYS_BRK => 0,
        SYS_SET_THREAD_AREA => {
            // SAFETY: setting the FS base takes no pointers.
            errno::result(unsafe { sys::thread_set_user_base(0, a1 as u64) }, 0)
        }
        SYS_SET_TID_ADDRESS => thread::set_tid_address(a1 as usize, TID.load(Ordering::Relaxed)),
        SYS_GETTID => thread::gettid(),
        SYS_FUTEX => futex::futex(a1 as *const u32, a2, a3, a4 as *const futex::Timeout, a6),
        SYS_RT_SIGACTION => signal::sigaction(
            a1,
            a2 as *const signal::KSigaction,
            a3 as *mut signal::KSigaction,
            a4 as u64,
        ),
        SYS_RT_SIGPROCMASK => signal::sigprocmask(a1, a2 as *const u64, a3 as *mut u64, a4 as u64),
        SYS_RT_SIGPENDING => signal::sigpending(a1 as *mut u64, a2 as u64),
        SYS_RT_SIGSUSPEND => signal::sigsuspend(a1 as *const u64, a2 as u64),
        SYS_PAUSE => signal::pause(),
        SYS_RT_SIGTIMEDWAIT => signal::sigtimedwait(
            a1 as *const u64,
            a2 as *mut u8,
            a3 as *const [i64; 2],
            a4 as u64,
        ),
        SYS_SIGALTSTACK => {
            signal::sigaltstack(a1 as *const signal::Stack, a2 as *mut signal::Stack)
        }
        SYS_GETPID => process::getpid(),
        SYS_GETPPID => process::getppid(),
        SYS_SETPGID => process::setpgid(a1, a2),
        SYS_GETPGID => process::getpgid(a1),
        SYS_SETSID => process::setsid(),
        SYS_GETSID => process::getsid(a1),
        // SAFETY: musl passes what `execve` takes.
        SYS_EXECVE => unsafe {
            spawn::execve(a1 as *const _, a2 as *const *const _, a3 as *const *const _)
        },
        SYS_WAIT4 => spawn::wait4(a1, a2 as *mut i32, a3, a4 as *mut u8),
        SYS_KILL => process::kill(a1, a2),
        SYS_TKILL => signal::tkill(a1, a2),
        SYS_TGKILL => signal::tkill(a2, a3),
        SYS_POLL => poll::poll(a1 as *mut poll::Pollfd, a2 as u64, a3),
        SYS_PPOLL => poll::ppoll(
            a1 as *mut poll::Pollfd,
            a2 as u64,
            a3 as *const clock::Timespec,
            a4 as *const u64,
        ),
        SYS_SELECT => poll::select(a1, a2 as *mut _, a3 as *mut _, a4 as *mut _, a5 as *const _),
        SYS_EPOLL_CREATE => epoll::create_sized(a1),
        SYS_EPOLL_CREATE1 => epoll::create(a1),
        SYS_TIMERFD_CREATE => timerfd::create(a1, a2),
        SYS_TIMERFD_SETTIME => timerfd::settime(
            a1,
            a2,
            a3 as *const timerfd::Itimerspec,
            a4 as *mut timerfd::Itimerspec,
        ),
        SYS_TIMERFD_GETTIME => timerfd::gettime(a1, a2 as *mut timerfd::Itimerspec),
        SYS_EPOLL_CTL => epoll::ctl(a1, a2, a3, a4 as *const epoll::Event),
        SYS_EPOLL_WAIT => epoll::pwait(a1, a2 as *mut epoll::Event, a3, a4, core::ptr::null(), 8),
        SYS_EPOLL_PWAIT => epoll::pwait(
            a1,
            a2 as *mut epoll::Event,
            a3,
            a4,
            a5 as *const u64,
            a6 as u64,
        ),
        SYS_PSELECT6 => poll::pselect6(
            a1,
            a2 as *mut _,
            a3 as *mut _,
            a4 as *mut _,
            a5 as *const clock::Timespec,
            a6 as *const [u64; 2],
        ),
        SYS_GETRANDOM => getrandom(a1 as *mut u8, a2 as u64),
        SYS_CLOCK_GETTIME => clock::gettime(a1, a2 as *mut clock::Timespec),
        SYS_NANOSLEEP => clock::nanosleep(
            clock::CLOCK_REALTIME,
            0,
            a1 as *const clock::Timespec,
            a2 as *mut clock::Timespec,
        ),
        SYS_CLOCK_NANOSLEEP => clock::nanosleep(
            a1,
            a2,
            a3 as *const clock::Timespec,
            a4 as *mut clock::Timespec,
        ),
        SYS_EXIT_GROUP => exit(a1),
        SYS_EXIT => thread::exit(),
        _ => -ENOSYS,
    }
}

pub(crate) fn process() -> Handle {
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

/// `mmap`: maps `len` bytes of a new anonymous memory object, or of the
/// file of descriptor `fd` from `offset`. A fixed mapping replaces
/// whatever was mapped in its range.
fn mmap(addr: u64, len: u64, prot: i64, flags: i64, fd: i64, offset: i64) -> i64 {
    if len == 0 {
        return -EINVAL;
    }
    let perms = match perms(prot) {
        Ok(perms) => perms,
        Err(e) => return -e,
    };
    let Some(size) = len.checked_next_multiple_of(PAGE_SIZE) else {
        return -errno::ENOMEM;
    };
    // Only the file is mapped from the object; anonymous memory needs a
    // new one.
    let file = if flags & MAP_ANONYMOUS == 0 {
        if offset < 0 || !(offset as u64).is_multiple_of(PAGE_SIZE) {
            return -EINVAL;
        }
        match file_memory(fd, prot, flags) {
            Ok(memory) => Some((memory, offset)),
            Err(e) => return -e,
        }
    } else {
        None
    };
    let fixed = flags & MAP_FIXED != 0;
    let mut options = 0;
    if fixed {
        options |= SPECIFIC;
        // SAFETY: unmapping takes no pointers. Nothing may be mapped
        // there, which is no error.
        unsafe { sys::vm_unmap(process(), addr, size) };
    }
    let at = if fixed { addr } else { 0 };
    let Some((memory, offset)) = file else {
        return match map_anonymous(options | perms, at, size) {
            Ok(mapped) => mapped as i64,
            Err(e) => -e,
        };
    };
    let offset = offset as u64;
    let in_file = memory_size(memory.raw()).saturating_sub(offset).min(len);
    if in_file == len {
        let mut mapped = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe {
            sys::vm_map(
                process(),
                options | perms,
                at,
                memory.raw(),
                offset,
                len,
                &mut mapped,
            )
        };
        return errno::result(status, mapped as i64);
    }
    // The range runs past the end of the file. It is reserved without
    // access, and the part in the file then mapped over the reservation.
    let base = match map_anonymous(options, at, size) {
        Ok(base) => base,
        Err(e) => return -e,
    };
    if in_file > 0 {
        let pages = in_file.next_multiple_of(PAGE_SIZE);
        let mut mapped = 0;
        // SAFETY: unmapping and mapping take no pointers but the out
        // pointer, which is valid for the call.
        let status = unsafe {
            sys::vm_unmap(process(), base, pages);
            let options = perms | SPECIFIC;
            sys::vm_map(
                process(),
                options,
                base,
                memory.raw(),
                offset,
                in_file,
                &mut mapped,
            )
        };
        if status != mikros_rt::OK {
            // SAFETY: unmapping takes no pointers.
            unsafe { sys::vm_unmap(process(), base, size) };
            return -errno::from_status(status);
        }
    }
    base as i64
}

/// Maps a new anonymous memory object of `size` bytes with `options` at
/// `at`, and returns its address. Without permissions, the mapping only
/// reserves its range.
fn map_anonymous(options: u32, at: u64, size: u64) -> Result<u64, i64> {
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::mem_create(size, 0, &mut mem) };
    if status != mikros_rt::OK {
        return Err(errno::from_status(status));
    }
    let mut mapped = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::vm_map(process(), options, at, mem, 0, size, &mut mapped) };
    // The mapping keeps the memory object alive.
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(mem) };
    match status {
        mikros_rt::OK => Ok(mapped),
        status => Err(errno::from_status(status)),
    }
}

/// The memory object of the file of descriptor `fd` for a mapping with
/// `prot` and `flags`: a private copy for `MAP_PRIVATE`.
fn file_memory(fd: i64, prot: i64, flags: i64) -> Result<OwnedHandle, i64> {
    let mut wanted = match flags & MAP_TYPE {
        MAP_PRIVATE => MemoryFlags::PRIVATE.0,
        MAP_SHARED | MAP_SHARED_VALIDATE => 0,
        _ => return Err(EINVAL),
    };
    if prot & PROT_WRITE != 0 {
        wanted |= MemoryFlags::WRITE.0;
    }
    if prot & PROT_EXEC != 0 {
        wanted |= MemoryFlags::EXECUTE.0;
    }
    let description = fd::get(fd).ok_or(EBADF)?;
    let Description::File(node) = &*description else {
        return Err(ENODEV);
    };
    match node
        .node
        .lock()
        .get_memory_object(MemoryFlags(wanted), FOREVER)
    {
        Ok(Ok(r)) => Ok(r.memory),
        Ok(Err(e)) => Err(errno::from_fs(e)),
        Err(_) => Err(errno::EIO),
    }
}

/// The size of memory object `mem`, or 0 when it cannot be inspected.
fn memory_size(mem: Handle) -> u64 {
    let mut record = [0u8; INFO_MEMORY_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status = unsafe {
        sys::object_get_info(
            mem,
            INFO_MEMORY,
            record.as_mut_ptr(),
            INFO_MEMORY_SIZE as u64,
            &mut len,
        )
    };
    if status != mikros_rt::OK || len < 8 {
        return 0;
    }
    u64::from_le_bytes(record[..8].try_into().expect("8 bytes"))
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
            let flags = match &*description {
                Description::File(node) | Description::Directory(node, _) => &node.flags,
                Description::Timer(timer) => &timer.flags,
                // Streams are read and written, and have no other flags,
                // nor has an epoll instance.
                Description::Stream(_) | Description::Epoll(_) => {
                    return if cmd == F_GETFL {
                        i64::from(fs::O_RDWR)
                    } else {
                        0
                    };
                }
            };
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
