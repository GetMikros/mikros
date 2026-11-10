//! Reading, writing and seeking descriptors. Streams use the stream
//! system calls, and wait while they are empty or full. Files use their
//! connection: `read`, `write` and `lseek` its cursor, and `pread` and
//! `pwrite` explicit offsets.

use crate::errno::{self, EBADF, EINVAL, EIO, EISDIR, EPIPE, ESPIPE};
use crate::fd::{self, Description};
use alloc::vec::Vec;
use mikros_rt::interfaces::mikros::fs::SeekOrigin;
use mikros_rt::sys::{self, Handle};

const READABLE: u32 = 1 << 0;
const WRITABLE: u32 = 1 << 1;
const PEER_CLOSED: u32 = 1 << 2;
const SHOULD_WAIT: sys::Status = -22;
const STATUS_PEER_CLOSED: sys::Status = -24;
/// The most bytes one file request carries.
const CHUNK_MAX: u64 = 32768;
const FOREVER: i64 = i64::MAX;
const SIGPIPE: u32 = 13;
pub const SEEK_SET: i64 = 0;
pub const SEEK_CUR: i64 = 1;
const SEEK_END: i64 = 2;

#[repr(C)]
pub struct Iovec {
    base: *mut u8,
    len: u64,
}

/// Waits until `stream` has one of `signals`.
fn wait(stream: Handle, signals: u32) -> Result<(), i64> {
    let mut observed = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::object_wait_one(stream, signals, FOREVER, &mut observed) };
    if status == mikros_rt::OK {
        Ok(())
    } else {
        Err(errno::from_status(status))
    }
}

pub fn read(fd: i64, buf: *mut u8, len: u64) -> i64 {
    let Some(description) = fd::get(fd) else {
        return -EBADF;
    };
    match &*description {
        Description::Stream(stream) => loop {
            let mut actual = 0;
            // SAFETY: musl passes a buffer of `len` bytes.
            let status = unsafe { sys::stream_read(stream.raw(), buf, len, &mut actual) };
            match status {
                SHOULD_WAIT => {
                    if let Err(e) = wait(stream.raw(), READABLE | PEER_CLOSED) {
                        return -e;
                    }
                }
                // The queue is empty and the peer is closed: the end.
                STATUS_PEER_CLOSED => return 0,
                status => return errno::result(status, actual as i64),
            }
        },
        Description::File(node) => {
            let count = len.min(CHUNK_MAX) as u32;
            match node.node.lock().read_next(count, FOREVER) {
                Ok(Ok(r)) => copy_out(&r.data, buf, len),
                Ok(Err(e)) => -errno::from_fs(e),
                Err(_) => -EIO,
            }
        }
        Description::Directory(..) => -EISDIR,
        Description::Epoll(_) => -EINVAL,
        Description::Timer(timer) => crate::timerfd::read(timer, buf, len),
        Description::Terminal(t) => t.read(buf, len),
    }
}

/// Copies `data`, at most `len` bytes of it, to `buf`.
fn copy_out(data: &[u8], buf: *mut u8, len: u64) -> i64 {
    let n = data.len().min(len as usize);
    // SAFETY: musl passes a buffer of `len` bytes, and `n` is at most
    // `len`.
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), buf, n) };
    n as i64
}

/// The `len` bytes at `buf`, at most one file request's worth.
fn copy_in(buf: *const u8, len: u64) -> Vec<u8> {
    let n = len.min(CHUNK_MAX) as usize;
    // SAFETY: musl passes a buffer of `len` bytes, and `n` is at most
    // `len`.
    unsafe { core::slice::from_raw_parts(buf, n) }.to_vec()
}

pub fn write(fd: i64, buf: *const u8, len: u64) -> i64 {
    let Some(description) = fd::get(fd) else {
        return -EBADF;
    };
    match &*description {
        Description::Stream(stream) => loop {
            let mut actual = 0;
            // SAFETY: musl passes a buffer of `len` bytes.
            let status = unsafe { sys::stream_write(stream.raw(), buf, len, &mut actual) };
            match status {
                SHOULD_WAIT => {
                    if let Err(e) = wait(stream.raw(), WRITABLE | PEER_CLOSED) {
                        return -e;
                    }
                }
                STATUS_PEER_CLOSED => {
                    // The calling thread gets SIGPIPE, as the write fails.
                    if let Some(thread) = crate::thread::current() {
                        // SAFETY: sending a signal takes no pointers.
                        unsafe { sys::thread_signal(thread, SIGPIPE, 0) };
                    }
                    return -EPIPE;
                }
                status => return errno::result(status, actual as i64),
            }
        },
        Description::File(node) => match node.node.lock().write_next(copy_in(buf, len), FOREVER) {
            Ok(Ok(r)) => i64::from(r.written),
            Ok(Err(e)) => -errno::from_fs(e),
            Err(_) => -EIO,
        },
        Description::Directory(..) => -EISDIR,
        Description::Epoll(_) | Description::Timer(_) => -EINVAL,
        Description::Terminal(t) => t.write(buf, len),
    }
}

pub fn pread(fd: i64, buf: *mut u8, len: u64, offset: i64) -> i64 {
    let Some(description) = fd::get(fd) else {
        return -EBADF;
    };
    let Ok(offset) = u64::try_from(offset) else {
        return -EINVAL;
    };
    match &*description {
        Description::File(node) => {
            match node
                .node
                .lock()
                .read(offset, len.min(CHUNK_MAX) as u32, FOREVER)
            {
                Ok(Ok(r)) => copy_out(&r.data, buf, len),
                Ok(Err(e)) => -errno::from_fs(e),
                Err(_) => -EIO,
            }
        }
        Description::Directory(..) => -EISDIR,
        Description::Stream(_)
        | Description::Terminal(_)
        | Description::Epoll(_)
        | Description::Timer(_) => -ESPIPE,
    }
}

pub fn pwrite(fd: i64, buf: *const u8, len: u64, offset: i64) -> i64 {
    let Some(description) = fd::get(fd) else {
        return -EBADF;
    };
    let Ok(offset) = u64::try_from(offset) else {
        return -EINVAL;
    };
    match &*description {
        Description::File(node) => match node.node.lock().write(offset, copy_in(buf, len), FOREVER)
        {
            Ok(Ok(r)) => i64::from(r.written),
            Ok(Err(e)) => -errno::from_fs(e),
            Err(_) => -EIO,
        },
        Description::Directory(..) => -EISDIR,
        Description::Stream(_)
        | Description::Terminal(_)
        | Description::Epoll(_)
        | Description::Timer(_) => -ESPIPE,
    }
}

pub fn lseek(fd: i64, offset: i64, whence: i64) -> i64 {
    let Some(description) = fd::get(fd) else {
        return -EBADF;
    };
    let origin = match whence {
        SEEK_SET => SeekOrigin::Start,
        SEEK_CUR => SeekOrigin::Current,
        SEEK_END => SeekOrigin::End,
        _ => return -EINVAL,
    };
    match &*description {
        Description::File(node) => match node.node.lock().seek(offset, origin, FOREVER) {
            Ok(Ok(r)) => r.position as i64,
            Ok(Err(e)) => -errno::from_fs(e),
            Err(_) => -EIO,
        },
        Description::Directory(_, listing) => crate::dir::seek(listing, offset, whence),
        Description::Stream(_)
        | Description::Terminal(_)
        | Description::Epoll(_)
        | Description::Timer(_) => -ESPIPE,
    }
}

/// `readv` and `writev`: the `count` buffers at `iov` in order, until
/// one is transferred only in part.
pub fn vectored(fd: i64, iov: *const Iovec, count: i64, writing: bool) -> i64 {
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
        let done = if writing {
            write(fd, v.base, v.len)
        } else {
            read(fd, v.base, v.len)
        };
        if done < 0 {
            return if total > 0 { total } else { done };
        }
        total += done;
        if (done as u64) < v.len {
            break;
        }
    }
    total
}
