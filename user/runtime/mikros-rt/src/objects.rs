//! Safe forms of the system calls on clocks, handles, events, memory
//! objects and random bytes that services make, so that their own code
//! needs no `unsafe`. Each takes slices for the kernel's pointers and
//! returns a handle the kernel created as an `OwnedHandle`.

use crate::interfaces::__midl::OwnedHandle;
use crate::sys::{self, Handle, Status};

fn status(s: Status) -> Result<(), Status> {
    match s {
        crate::OK => Ok(()),
        s => Err(s),
    }
}

/// A handle that a successful call just created, `raw`.
fn created(s: Status, raw: Handle) -> Result<OwnedHandle, Status> {
    status(s)?;
    // SAFETY: the kernel just created the handle, and nothing else holds
    // its number.
    Ok(unsafe { OwnedHandle::from_raw(raw) })
}

/// The monotonic clock, in nanoseconds.
pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// A new handle to the object of `handle` with `rights`.
pub fn duplicate(handle: Handle, rights: u32) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let s = unsafe { sys::handle_duplicate(handle, rights, &mut out) };
    created(s, out)
}

/// Clears the signals `clear` of the object of `handle` and sets the
/// signals `set`.
pub fn signal(handle: Handle, clear: u32, set: u32) -> Result<(), Status> {
    // SAFETY: the call takes no pointers.
    status(unsafe { sys::object_signal(handle, clear, set) })
}

/// A new event with `options`.
pub fn event(options: u32) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let s = unsafe { sys::event_create(options, &mut out) };
    created(s, out)
}

/// A new memory object of `size` bytes with `options`.
pub fn memory(size: u64, options: u32) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let s = unsafe { sys::mem_create(size, options, &mut out) };
    created(s, out)
}

/// The bytes of memory that the kernel's frame allocator manages.
pub fn system_memory() -> u64 {
    let mut record = [0u8; 16];
    // SAFETY: the record holds the 16 bytes the call writes.
    unsafe { sys::system_get_memory_info(record.as_mut_ptr()) };
    u64::from_le_bytes(record[..8].try_into().expect("8 bytes")) * 4096
}

/// Fills `buf` from the memory object `memory`, from `offset`.
pub fn read_memory(memory: Handle, offset: u64, buf: &mut [u8]) -> Result<(), Status> {
    // SAFETY: the pointer and length describe `buf`.
    status(unsafe { sys::mem_read(memory, offset, buf.as_mut_ptr(), buf.len() as u64) })
}

/// Writes `data` to the memory object `memory` at `offset`.
pub fn write_memory(memory: Handle, offset: u64, data: &[u8]) -> Result<(), Status> {
    // SAFETY: the pointer and length describe `data`.
    status(unsafe { sys::mem_write(memory, offset, data.as_ptr(), data.len() as u64) })
}

/// Fills `record` with the record `topic` of `handle`, and returns the
/// length the kernel gave.
pub fn info(handle: Handle, topic: u32, record: &mut [u8]) -> Result<usize, Status> {
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let s = unsafe {
        sys::object_get_info(
            handle,
            topic,
            record.as_mut_ptr(),
            record.len() as u64,
            &mut len,
        )
    };
    status(s)?;
    Ok(len as usize)
}

/// Fills `buf` with random bytes from the kernel.
pub fn random(buf: &mut [u8]) -> Result<(), Status> {
    // SAFETY: the pointer and length describe `buf`.
    status(unsafe { sys::system_get_random(buf.as_mut_ptr(), buf.len() as u64) })
}

/// Overwrites `bytes` with zeros in a way the compiler keeps, for
/// secrets that must not stay in memory.
pub fn wipe(bytes: &mut [u8]) {
    for b in bytes {
        // SAFETY: `b` is a valid, aligned byte.
        unsafe { core::ptr::write_volatile(b, 0) };
    }
}
