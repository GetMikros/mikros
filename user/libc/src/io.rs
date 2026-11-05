//! Reading and writing descriptors. Streams use the stream system calls,
//! and wait while they are empty or full.

use crate::errno::{self, EBADF, EINVAL, EPIPE};
use crate::fd::{self, Description};
use mikros_rt::sys::{self, Handle};

const READABLE: u32 = 1 << 0;
const WRITABLE: u32 = 1 << 1;
const PEER_CLOSED: u32 = 1 << 2;
const SHOULD_WAIT: sys::Status = -22;
const STATUS_PEER_CLOSED: sys::Status = -24;
const FOREVER: i64 = i64::MAX;

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
    }
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
                STATUS_PEER_CLOSED => return -EPIPE,
                status => return errno::result(status, actual as i64),
            }
        },
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
