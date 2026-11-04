//! Reading what the bootloader left in memory.

use crate::bootinfo;
use boot_protocol::{BootInfo, MemoryMapEntry};

/// The memory map entries of `info`, read through the direct map.
pub fn memory_map(info: &BootInfo) -> impl Iterator<Item = MemoryMapEntry> + Clone {
    let size = bootinfo::memory_map_size(info.memory_map_count, info.memory_map_entry_size)
        .ok()
        .flatten()
        .unwrap_or_else(|| panic!("the memory map size is invalid"));
    let address = info.direct_map_base + info.memory_map_phys;
    // SAFETY: the bootloader places the memory map in BootloaderReclaimable
    // memory covered by the direct map, and nothing frees that memory
    // while the kernel reads it.
    let bytes = unsafe { core::slice::from_raw_parts(address as *const u8, size) };
    bootinfo::memory_map(bytes, info.memory_map_entry_size).unwrap_or_else(|e| panic!("{e}"))
}
