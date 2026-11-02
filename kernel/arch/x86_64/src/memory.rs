//! Physical memory at boot.

use boot_protocol::{BootInfo, Kind, MemoryMapEntry, PAGE_SIZE};
use kernel_core::bootinfo;
use kernel_core::frames::{self, Frames};
use kernel_core::sync::SpinLock;

/// The frame allocator, set up by `init`.
pub static FRAMES: SpinLock<Option<Frames<'static>>> = SpinLock::new(None);

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

/// Builds the frame allocator from the `Usable` ranges of the memory map,
/// keeping its bitmap in the first such range large enough to hold it.
/// Returns the number of free frames.
pub fn init(info: &BootInfo) -> u64 {
    let map = memory_map(info);
    let words = frames::bitmap_words(map.clone());
    let pages = (words as u64 * 8).div_ceil(PAGE_SIZE);
    let home = map
        .clone()
        .find(|e| e.kind == Kind::Usable as u32 && e.page_count >= pages)
        .unwrap_or_else(|| panic!("no memory for the frame bitmap of {pages} pages"));
    let address = info.direct_map_base + home.phys_start;
    // SAFETY: the range is Usable, so nothing else uses it, it lies in the
    // direct map, and it holds at least `words` words. The bitmap's frames
    // are reserved below, so the allocator never hands them out.
    let bits = unsafe { core::slice::from_raw_parts_mut(address as *mut u64, words) };
    let mut allocator = Frames::new(bits);
    allocator.add_kind(map, Kind::Usable);
    allocator.reserve(home.phys_start / PAGE_SIZE, pages);
    let free = allocator.free_count();
    *FRAMES.lock() = Some(allocator);
    free
}
