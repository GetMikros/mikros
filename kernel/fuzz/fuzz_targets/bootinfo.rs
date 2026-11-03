//! Decodes `BootInfo` and its memory map, and checks the framebuffer
//! against a model that merges the map's `Framebuffer` ranges.

#![no_main]

use boot_protocol::{Framebuffer, Kind, MemoryMapEntry};
use kernel_core::bootinfo::{self, FramebufferError, HEADER_SIZE};
use libfuzzer_sys::fuzz_target;

/// The result `check_framebuffer` must give, worked out in 128 bits.
fn model(fb: &Framebuffer, map: &[MemoryMapEntry]) -> Result<(), FramebufferError> {
    if fb.width == 0 || fb.height == 0 {
        return Err(FramebufferError::Empty);
    }
    if fb.stride < fb.width {
        return Err(FramebufferError::NarrowStride);
    }
    let lines = u128::from(fb.stride) * u128::from(fb.height) * 4;
    if lines > u128::from(u64::MAX) || lines > u128::from(fb.size) {
        return Err(FramebufferError::TooSmall);
    }
    let (start, end) = (u128::from(fb.phys_base), u128::from(fb.phys_base) + u128::from(fb.size));
    if end > u128::from(u64::MAX) {
        return Err(FramebufferError::RangeOverflow);
    }
    // Ranges whose end overflows are left out, as the kernel cannot
    // address them.
    let mut ranges: Vec<(u128, u128)> = map
        .iter()
        .filter(|e| e.kind == Kind::Framebuffer as u32)
        .map(|e| (u128::from(e.phys_start), u128::from(e.phys_start) + u128::from(e.page_count) * 4096))
        .filter(|&(_, e)| e <= u128::from(u64::MAX))
        .collect();
    ranges.sort();
    let mut covered = start;
    for (s, e) in ranges {
        if s <= covered && e > covered {
            covered = e;
        }
    }
    if covered >= end { Ok(()) } else { Err(FramebufferError::NotInMemoryMap) }
}

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
        assert_eq!(entries.clone().count() as u64, info.memory_map_count);
        let list: Vec<MemoryMapEntry> = entries.clone().collect();
        assert_eq!(
            bootinfo::check_framebuffer(&info.framebuffer, entries),
            model(&info.framebuffer, &list)
        );
    }
});
