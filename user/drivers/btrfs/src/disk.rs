// SPDX-License-Identifier: GPL-2.0
//! The partition under the file system, which the shim reads and writes
//! through the host layer's disk calls: its `Block` channel, through one
//! buffer mapped into the server and cut into slots, one for each
//! request in flight, so that the shim's workers' requests can run at
//! once.

use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::interfaces::mikros::storage::{BlockClient, BlockFlags, BlockInfo};
use mikros_rt::sys;

const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const FOREVER: i64 = i64::MAX;
/// The requests in flight at most, and the most bytes each moves.
const SLOTS: usize = 16;
const SLOT_MAX: u64 = 1 << 20;
const EIO: c_int = 5;
const EINVAL: c_int = 22;

pub struct Disk {
    block: BlockClient,
    buffer: u32,
    at: u64,
    slot_size: u64,
    pub info: BlockInfo,
    /// A bit for each slot in use, and a count that changes when one
    /// is given back, which takers wait on.
    used: AtomicU32,
    freed: AtomicU32,
}

impl Disk {
    /// The disk behind `block`, with its buffer mapped into `process`.
    /// None when the disk cannot give one.
    pub fn new(block: BlockClient, process: sys::Handle) -> Option<Disk> {
        let info = block.get_info(FOREVER).ok()?.info;
        let block_size = u64::from(info.block_size);
        if block_size == 0 || !block_size.is_power_of_two() || block_size > 4096 {
            return None;
        }
        let max = u64::from(info.max_transfer_blocks.max(1)) * block_size;
        let slot_size = max.min(SLOT_MAX) / block_size * block_size;
        let size = slot_size * SLOTS as u64;
        let created = block.create_buffer(size, FOREVER).ok()?.ok()?;
        let mut at = 0;
        // SAFETY: the out pointer is valid; the kernel picks a free range.
        let mapped = unsafe {
            sys::vm_map(
                process,
                PERM_READ | PERM_WRITE,
                0,
                created.memory.raw(),
                0,
                size,
                &mut at,
            )
        };
        if mapped != mikros_rt::OK {
            return None;
        }
        Some(Disk {
            block,
            buffer: created.buffer,
            at,
            slot_size,
            info,
            used: AtomicU32::new(0),
            freed: AtomicU32::new(0),
        })
    }

    pub fn size(&self) -> u64 {
        self.info.block_count * u64::from(self.info.block_size)
    }

    pub fn read_only(&self) -> bool {
        self.info.flags.0 & BlockFlags::READ_ONLY.0 != 0
    }

    /// Whether the disk takes `Discard`.
    pub fn discards(&self) -> bool {
        self.info.flags.0 & BlockFlags::DISCARD.0 != 0
    }

    /// A free slot, once there is one.
    fn take(&self) -> usize {
        loop {
            let freed = self.freed.load(Ordering::Acquire);
            let used = self.used.load(Ordering::Acquire);
            let slot = (!used).trailing_zeros() as usize;
            if slot < SLOTS {
                let bit = 1 << slot;
                if self.used.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
                    return slot;
                }
                continue;
            }
            // SAFETY: the word stays valid while the thread waits on it.
            unsafe { sys::futex_wait(self.freed.as_ptr(), freed, FOREVER) };
        }
    }

    fn give_back(&self, slot: usize) {
        self.used.fetch_and(!(1 << slot), Ordering::Release);
        self.freed.fetch_add(1, Ordering::Release);
        let mut woken = 0;
        // SAFETY: as for waiting.
        unsafe { sys::futex_wake(self.freed.as_ptr(), 1, &mut woken) };
    }

    /// Where a slot starts in the server's mapping of the buffer.
    fn slot(&self, slot: usize) -> *mut u8 {
        (self.at + slot as u64 * self.slot_size) as *mut u8
    }

    /// Reads or writes `len` bytes at `offset`, whole blocks, with `buf`,
    /// a slot's worth at a time: 0, or a negative errno.
    fn transfer(&self, write: bool, mut offset: u64, buf: *mut u8, len: usize) -> c_int {
        let block_size = u64::from(self.info.block_size);
        if !offset.is_multiple_of(block_size) || !(len as u64).is_multiple_of(block_size) {
            return -EINVAL;
        }
        let slot = self.take();
        let mut done = 0;
        let mut ret = 0;
        while done < len {
            let n = (len - done).min(self.slot_size as usize);
            let (block, count) = (offset / block_size, (n as u64 / block_size) as u32);
            let at = slot as u64 * self.slot_size;
            // SAFETY: the shim's buffer holds `len` bytes, which nothing
            // else touches during the call, and the slot `slot_size`
            // bytes, which only the thread that took it touches until it
            // gives it back.
            let ok = if write {
                unsafe { core::ptr::copy_nonoverlapping(buf.add(done), self.slot(slot), n) };
                matches!(
                    self.block
                        .write(self.buffer, at, block, count, false, FOREVER),
                    Ok(Ok(_))
                )
            } else {
                let ok = matches!(
                    self.block.read(self.buffer, at, block, count, FOREVER),
                    Ok(Ok(_))
                );
                if ok {
                    unsafe { core::ptr::copy_nonoverlapping(self.slot(slot), buf.add(done), n) };
                }
                ok
            };
            if !ok {
                ret = -EIO;
                break;
            }
            done += n;
            offset += n as u64;
        }
        self.give_back(slot);
        ret
    }

    fn flush(&self) -> c_int {
        match self.block.flush(FOREVER) {
            Ok(Ok(_)) => 0,
            _ => -EIO,
        }
    }

    /// Discards `len` bytes at `offset`, whole blocks: 0, or a negative
    /// errno.
    fn discard(&self, offset: u64, len: u64) -> c_int {
        let block_size = u64::from(self.info.block_size);
        if !offset.is_multiple_of(block_size) || !len.is_multiple_of(block_size) {
            return -EINVAL;
        }
        match self
            .block
            .discard(offset / block_size, len / block_size, FOREVER)
        {
            Ok(Ok(_)) => 0,
            _ => -EIO,
        }
    }
}

/// The disk of a handle that `main` gave the shim.
fn disk<'a>(handle: *mut c_void) -> &'a Disk {
    // SAFETY: the shim passes back the handle, which points to a disk
    // that stays for the rest of the program.
    unsafe { &*handle.cast::<Disk>() }
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_read(
    handle: *mut c_void,
    offset: u64,
    buf: *mut u8,
    len: usize,
) -> c_int {
    disk(handle).transfer(false, offset, buf, len)
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_write(
    handle: *mut c_void,
    offset: u64,
    buf: *const u8,
    len: usize,
) -> c_int {
    disk(handle).transfer(true, offset, buf.cast_mut(), len)
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_flush(handle: *mut c_void) -> c_int {
    disk(handle).flush()
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_discard(handle: *mut c_void, offset: u64, len: u64) -> c_int {
    disk(handle).discard(offset, len)
}
