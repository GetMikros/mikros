//! `sysinfo` (MIKSPEC-0033.R47): the frames that the kernel's frame
//! allocator manages and those of them that are free (MIKSPEC-0006.R34),
//! as `totalram` and `freeram` in pages of 4096 bytes, with every other
//! field 0. musl's `sysconf` takes `_SC_PHYS_PAGES` and `_SC_AVPHYS_PAGES`
//! from them, and `get_phys_pages` and `get_avphys_pages` from `sysconf`.

use crate::errno::EFAULT;
use mikros_rt::sys;

/// The size of musl's `struct sysinfo` on x86_64, and the offsets of its
/// fields `totalram`, `freeram` and `mem_unit`.
const SIZE: usize = 368;
const TOTALRAM: usize = 32;
const FREERAM: usize = 40;
const MEM_UNIT: usize = 104;
const PAGE_SIZE: u32 = 4096;

pub fn sysinfo(info: *mut u8) -> i64 {
    if info.is_null() {
        return -EFAULT;
    }
    let mut record = [0u8; 16];
    // SAFETY: the record holds the 16 bytes that the call writes.
    unsafe { sys::system_get_memory_info(record.as_mut_ptr()) };
    let mut fields = [0u8; SIZE];
    fields[TOTALRAM..TOTALRAM + 8].copy_from_slice(&record[..8]);
    fields[FREERAM..FREERAM + 8].copy_from_slice(&record[8..]);
    fields[MEM_UNIT..MEM_UNIT + 4].copy_from_slice(&PAGE_SIZE.to_le_bytes());
    // SAFETY: musl passes a `struct sysinfo`, which is as large.
    unsafe { core::ptr::copy_nonoverlapping(fields.as_ptr(), info, SIZE) };
    0
}
