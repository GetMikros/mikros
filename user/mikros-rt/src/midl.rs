//! The system calls that generated MIDL bindings make, in the form their
//! runtime module expects.

use crate::sys::{self, Handle, Status};
use alloc::vec;
use alloc::vec::Vec;

pub const SHOULD_WAIT: i32 = -22;
pub const PEER_CLOSED: i32 = -24;
const BUFFER_TOO_SMALL: Status = -15;
const TIMED_OUT: Status = -21;
const INTERRUPTED: Status = -27;
const PEER_CLOSED_STATUS: Status = PEER_CLOSED as Status;

/// The largest message, in bytes and in handles.
const BYTES_MAX: usize = 65536;
const HANDLES_MAX: usize = 64;

const INFO_BASIC: u32 = 1;
const INFO_BASIC_SIZE: usize = 24;
const SIGNAL_READABLE: u32 = 1 << 0;
const SIGNAL_PEER_CLOSED: u32 = 1 << 2;

fn status(s: Status) -> Result<(), i32> {
    if s == crate::OK {
        Ok(())
    } else {
        Err(s as i32)
    }
}

fn close_all(handles: &[u32]) {
    for &h in handles {
        // SAFETY: closing takes no pointers.
        unsafe { sys::handle_close(h) };
    }
}

pub fn handle_close(handle: u32) {
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(handle) };
}

/// The object type and rights of `handle`, or `None` when it cannot be
/// inspected.
pub fn handle_info(handle: u32) -> Option<(u32, u32)> {
    let mut record = [0u8; INFO_BASIC_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let s = unsafe {
        sys::object_get_info(
            handle,
            INFO_BASIC,
            record.as_mut_ptr(),
            INFO_BASIC_SIZE as u64,
            &mut len,
        )
    };
    if s != crate::OK || len < INFO_BASIC_SIZE as u64 {
        return None;
    }
    let at = |i: usize| u32::from_le_bytes(record[i..i + 4].try_into().expect("4 bytes"));
    Some((at(16), at(20)))
}

pub fn handle_replace(handle: u32, rights: u32) -> Result<u32, i32> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    status(unsafe { sys::handle_replace(handle, rights, &mut out) })?;
    Ok(out)
}

pub fn channel_create() -> Result<(u32, u32), i32> {
    let (mut a, mut b) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    status(unsafe { sys::channel_create(0, &mut a, &mut b) })?;
    Ok((a, b))
}

/// Writes a message, consuming `handles` even when it fails.
pub fn channel_write(channel: u32, bytes: &[u8], handles: &[u32]) -> Result<(), i32> {
    // SAFETY: the pointers and lengths describe the slices.
    let s = unsafe {
        sys::channel_write(
            channel,
            bytes.as_ptr(),
            bytes.len() as u64,
            handles.as_ptr(),
            handles.len() as u64,
        )
    };
    if s != crate::OK {
        // A failed write leaves the handles with the caller.
        close_all(handles);
    }
    status(s)
}

pub fn channel_read(channel: u32) -> Result<(Vec<u8>, Vec<u32>), i32> {
    let mut bytes = vec![0u8; BYTES_MAX];
    let mut handles: Vec<Handle> = vec![0; HANDLES_MAX];
    let (mut len, mut count) = (0u64, 0u64);
    // SAFETY: the pointers and capacities describe the buffers.
    status(unsafe {
        sys::channel_read(
            channel,
            bytes.as_mut_ptr(),
            BYTES_MAX as u64,
            handles.as_mut_ptr(),
            HANDLES_MAX as u64,
            &mut len,
            &mut count,
        )
    })?;
    bytes.truncate(len as usize);
    handles.truncate(count as usize);
    Ok((bytes, handles))
}

/// Makes a call, consuming `handles` even when it fails, and returns the
/// reply. The kernel replaces the request's transaction ID.
pub fn channel_call(
    channel: u32,
    bytes: &[u8],
    handles: &[u32],
    deadline: i64,
) -> Result<(Vec<u8>, Vec<u32>), i32> {
    let mut reply = vec![0u8; BYTES_MAX];
    let mut reply_handles: Vec<Handle> = vec![0; HANDLES_MAX];
    let args: [u64; 8] = [
        bytes.as_ptr() as u64,
        bytes.len() as u64,
        handles.as_ptr() as u64,
        handles.len() as u64,
        reply.as_mut_ptr() as u64,
        BYTES_MAX as u64,
        reply_handles.as_mut_ptr() as u64,
        HANDLES_MAX as u64,
    ];
    let mut out = [0u64; 2];
    // SAFETY: the arguments and the buffers they point to are valid.
    let s = unsafe {
        sys::channel_call(
            channel,
            0,
            deadline,
            args.as_ptr().cast(),
            out.as_mut_ptr().cast(),
        )
    };
    match s {
        crate::OK => {}
        // The request was written, so its handles are gone.
        BUFFER_TOO_SMALL | TIMED_OUT | INTERRUPTED => return Err(s as i32),
        // The peer may have closed before or after the request was
        // written. The handles stay open rather than risk closing
        // numbers the kernel has given to other handles since.
        PEER_CLOSED_STATUS => return Err(s as i32),
        _ => {
            close_all(handles);
            return Err(s as i32);
        }
    }
    reply.truncate(out[0] as usize);
    reply_handles.truncate(out[1] as usize);
    Ok((reply, reply_handles))
}

/// Waits until `channel` is readable or its peer closed.
pub fn wait_readable(channel: u32, deadline: i64) -> Result<(), i32> {
    let mut observed = 0;
    // SAFETY: the out pointer is valid for the call.
    status(unsafe {
        sys::object_wait_one(
            channel,
            SIGNAL_READABLE | SIGNAL_PEER_CLOSED,
            deadline,
            &mut observed,
        )
    })
}
