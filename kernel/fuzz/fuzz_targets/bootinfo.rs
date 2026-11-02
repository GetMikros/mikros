//! Decodes `BootInfo` and its memory map.

#![no_main]

use kernel_core::bootinfo::{self, HEADER_SIZE};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < HEADER_SIZE {
        return;
    }
    let Ok(size) = bootinfo::size(&data[..HEADER_SIZE]) else {
        return;
    };
    let Some(whole) = data.get(..size) else {
        return;
    };
    let Ok(info) = bootinfo::decode(whole) else {
        return;
    };
    // The memory map follows the structure in the input.
    let map = &data[size..];
    if let Ok(Some(len)) = bootinfo::memory_map_size(info.memory_map_count, info.memory_map_entry_size)
        && let Some(map) = map.get(..len)
        && let Ok(entries) = bootinfo::memory_map(map, info.memory_map_entry_size)
    {
        assert_eq!(entries.count() as u64, info.memory_map_count);
    }
});
