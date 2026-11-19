//! Memory descriptors: `memfd_create`'s anonymous, resizable memory
//! objects, and those another process passed. The C library keeps each
//! description's byte size and offset; the object always holds whole
//! pages, at least one, enough for the byte size. Reading and writing
//! use `mem_read` and `mem_write`, never past the byte size, and a write
//! past the end grows it. Seals are the kernel's.

use crate::errno::{self, EBADF, EBUSY, EFAULT, EINVAL, EPERM};
use crate::fd::{self, Description};
use crate::lock::Lock;
use alloc::sync::Arc;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys::{self, Handle};

const PAGE_SIZE: u64 = 4096;
/// `RESIZABLE`, for `mem_create`.
const RESIZABLE: u32 = 1 << 0;
/// The `memfd_create` flags.
const MFD_CLOEXEC: u32 = 1;
const MFD_ALLOW_SEALING: u32 = 2;
/// The `F_SEAL_` bits, which are the kernel's seal bits in the same order.
pub const F_SEAL_SEAL: u32 = 1;
const F_SEAL_SHRINK: u32 = 2;
const F_SEAL_GROW: u32 = 4;
const F_SEAL_WRITE: u32 = 8;
const SEALS: u32 = F_SEAL_SEAL | F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE;
/// The memory information record, and where its size and seals are.
const INFO_MEMORY: u32 = 4;
const INFO_MEMORY_SIZE: usize = 0x18;
const STATUS_BAD_STATE: sys::Status = -20;
/// The most bytes one `mem_read` or `mem_write` moves.
const CHUNK: u64 = 1 << 20;

/// The byte size and the offset of `read` and `write`.
#[derive(Clone, Copy)]
struct State {
    size: u64,
    offset: u64,
}

pub struct Memory {
    pub object: OwnedHandle,
    state: Lock<State>,
}

/// The size and seals of memory object `object`.
fn info(object: Handle) -> Option<(u64, u32)> {
    let mut record = [0u8; INFO_MEMORY_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status = unsafe {
        sys::object_get_info(
            object,
            INFO_MEMORY,
            record.as_mut_ptr(),
            INFO_MEMORY_SIZE as u64,
            &mut len,
        )
    };
    if status != mikros_rt::OK || (len as usize) < INFO_MEMORY_SIZE {
        return None;
    }
    let size = u64::from_le_bytes(record[..8].try_into().ok()?);
    let seals = u32::from_le_bytes(record[0x10..0x14].try_into().ok()?);
    Some((size, seals))
}

/// The object's size for a byte size of `size`: whole pages, at least
/// one.
fn pages_for(size: u64) -> Option<u64> {
    size.max(1).checked_next_multiple_of(PAGE_SIZE)
}

impl Memory {
    /// The description of a memory object that another process passed,
    /// whose byte size is the object's size.
    pub fn received(object: OwnedHandle) -> Memory {
        let size = info(object.raw()).map_or(0, |(size, _)| size);
        Memory {
            object,
            state: Lock::new(State { size, offset: 0 }),
        }
    }

    pub fn size(&self) -> u64 {
        self.state.lock().size
    }

    /// Reads into the `len` bytes at `buf` from `offset`, at most to the
    /// byte size, and returns how many it read.
    fn read_at(&self, buf: *mut u8, len: u64, offset: u64) -> i64 {
        let size = self.state.lock().size;
        let count = size.saturating_sub(offset).min(len);
        let mut done = 0;
        while done < count {
            let n = (count - done).min(CHUNK);
            // SAFETY: the caller's buffer holds `len` bytes, and `done + n`
            // stays within it.
            let status = unsafe {
                sys::mem_read(self.object.raw(), offset + done, buf.add(done as usize), n)
            };
            if status != mikros_rt::OK {
                return -errno::from_status(status);
            }
            done += n;
        }
        done as i64
    }

    /// Writes the `len` bytes at `buf` at `offset`, growing the byte size
    /// and the object when they reach past the end.
    fn write_at(&self, buf: *const u8, len: u64, offset: u64) -> i64 {
        let Some(end) = offset.checked_add(len) else {
            return -EINVAL;
        };
        if end > self.state.lock().size {
            let grown = self.resize(end, false);
            if grown < 0 {
                return grown;
            }
        }
        let mut done = 0;
        while done < len {
            let n = (len - done).min(CHUNK);
            // SAFETY: the caller's buffer holds `len` bytes, and `done + n`
            // stays within it.
            let status = unsafe {
                sys::mem_write(self.object.raw(), offset + done, buf.add(done as usize), n)
            };
            if status != mikros_rt::OK {
                return -errno::from_status(status);
            }
            done += n;
        }
        len as i64
    }

    /// Sets the byte size to `size`, resizing the object to the pages
    /// that hold it and clearing the rest of its last page when it
    /// shrinks. With `seals`, the seals against shrinking and growing are
    /// checked first, as `ftruncate` does.
    fn resize(&self, size: u64, seals: bool) -> i64 {
        let mut state = self.state.lock();
        if seals && let Some((_, sealed)) = info(self.object.raw()) {
            let shrinking = size < state.size && sealed & F_SEAL_SHRINK != 0;
            let growing = size > state.size && sealed & F_SEAL_GROW != 0;
            if shrinking || growing {
                return -EPERM;
            }
        }
        let Some(pages) = pages_for(size) else {
            return -EINVAL;
        };
        if pages != pages_for(state.size).unwrap_or(0) {
            // SAFETY: the call takes no pointers.
            let status = unsafe { sys::mem_set_size(self.object.raw(), pages) };
            if status != mikros_rt::OK {
                return -errno::from_status(status);
            }
        }
        if size < state.size {
            let zeros = [0u8; 512];
            let mut at = size;
            while at < pages {
                let n = (pages - at).min(zeros.len() as u64);
                // SAFETY: the pointer and length describe `zeros`.
                let status = unsafe { sys::mem_write(self.object.raw(), at, zeros.as_ptr(), n) };
                if status != mikros_rt::OK {
                    return -errno::from_status(status);
                }
                at += n;
            }
        }
        state.size = size;
        0
    }

    pub fn read(&self, buf: *mut u8, len: u64) -> i64 {
        let offset = self.state.lock().offset;
        let read = self.read_at(buf, len, offset);
        if read > 0 {
            self.state.lock().offset = offset + read as u64;
        }
        read
    }

    pub fn write(&self, buf: *const u8, len: u64) -> i64 {
        let offset = self.state.lock().offset;
        let written = self.write_at(buf, len, offset);
        if written > 0 {
            self.state.lock().offset = offset + written as u64;
        }
        written
    }

    pub fn pread(&self, buf: *mut u8, len: u64, offset: u64) -> i64 {
        self.read_at(buf, len, offset)
    }

    pub fn pwrite(&self, buf: *const u8, len: u64, offset: u64) -> i64 {
        self.write_at(buf, len, offset)
    }

    /// Moves the offset as `lseek` does, with `SEEK_END` from the byte
    /// size.
    pub fn seek(&self, offset: i64, whence: i64) -> i64 {
        let mut state = self.state.lock();
        let base = match whence {
            crate::io::SEEK_SET => 0,
            crate::io::SEEK_CUR => state.offset as i64,
            crate::io::SEEK_END => state.size as i64,
            _ => return -EINVAL,
        };
        match base.checked_add(offset).filter(|&o| o >= 0) {
            Some(o) => {
                state.offset = o as u64;
                o
            }
            None => -EINVAL,
        }
    }

    pub fn truncate(&self, size: u64) -> i64 {
        self.resize(size, true)
    }

    /// `F_ADD_SEALS` with the `F_SEAL_` bits `seals`.
    pub fn add_seals(&self, seals: u32) -> i64 {
        if seals & !SEALS != 0 {
            return -EINVAL;
        }
        // SAFETY: the call takes no pointers.
        match unsafe { sys::mem_seal(self.object.raw(), seals) } {
            mikros_rt::OK => 0,
            STATUS_BAD_STATE => -EBUSY,
            status => -errno::from_status(status),
        }
    }

    /// `F_GET_SEALS`.
    pub fn seals(&self) -> i64 {
        info(self.object.raw()).map_or(-EINVAL, |(_, seals)| i64::from(seals & SEALS))
    }

    /// The object to map for `mmap`: the object itself when shared, a
    /// copy-on-write clone of the `len` bytes from `offset` when private.
    pub fn mapped(&self, private: bool, offset: u64, len: u64) -> Result<OwnedHandle, i64> {
        if !private {
            let mut out = 0;
            // SAFETY: the out pointer is valid for the call.
            let status = unsafe { sys::handle_duplicate(self.object.raw(), 1 << 31, &mut out) };
            if status != mikros_rt::OK {
                return Err(errno::from_status(status));
            }
            // SAFETY: the kernel just created the handle for us.
            return Ok(unsafe { OwnedHandle::from_raw(out) });
        }
        let len = len.checked_next_multiple_of(PAGE_SIZE).ok_or(EINVAL)?;
        let mut out = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::mem_clone(self.object.raw(), 0, offset, len, &mut out) };
        if status != mikros_rt::OK {
            return Err(errno::from_status(status));
        }
        // SAFETY: the kernel just created the handle for us.
        Ok(unsafe { OwnedHandle::from_raw(out) })
    }
}

/// `memfd_create`: a memory descriptor of a new object of one page with
/// a byte size of 0, sealed against further seals unless `flags` allows
/// sealing.
pub fn create(name: *const u8, flags: u32) -> i64 {
    if name.is_null() {
        return -EFAULT;
    }
    if flags & !(MFD_CLOEXEC | MFD_ALLOW_SEALING) != 0 {
        return -EINVAL;
    }
    let mut raw = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::mem_create(PAGE_SIZE, RESIZABLE, &mut raw) };
    if status != mikros_rt::OK {
        return -errno::from_status(status);
    }
    // SAFETY: the kernel just created the handle for us.
    let object = unsafe { OwnedHandle::from_raw(raw) };
    if flags & MFD_ALLOW_SEALING == 0 {
        // SAFETY: the call takes no pointers.
        let status = unsafe { sys::mem_seal(object.raw(), F_SEAL_SEAL) };
        if status != mikros_rt::OK {
            return -errno::from_status(status);
        }
    }
    let memory = Memory {
        object,
        state: Lock::new(State { size: 0, offset: 0 }),
    };
    fd::insert(
        Arc::new(Description::Memory(memory)),
        flags & MFD_CLOEXEC != 0,
        0,
    )
}

/// `fcntl` with `F_ADD_SEALS` or, when `add` is false, `F_GET_SEALS`.
pub fn fcntl_seals(fd: i64, add: bool, seals: i64) -> i64 {
    match fd::get(fd).as_deref() {
        Some(Description::Memory(m)) if add => m.add_seals(seals as u32),
        Some(Description::Memory(m)) => m.seals(),
        Some(_) => -EINVAL,
        None => -EBADF,
    }
}
