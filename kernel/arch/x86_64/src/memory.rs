//! Physical memory at boot.

use boot_protocol::{BootInfo, Kind, MemoryMapEntry, PAGE_SIZE};
use kernel_core::bootinfo;
use kernel_core::frames::{self, Frames};
use kernel_core::sync::SpinLock;

/// The frame allocator, set up by `init`.
pub static FRAMES: SpinLock<Option<Frames<'static>>> = SpinLock::new(None);

/// A frame from the frame allocator, or `None` when none is free.
pub fn alloc_frame() -> Option<u64> {
    #[cfg(feature = "fuzz")]
    if crate::fuzz::alloc_fails(crate::fuzz::FRAMES) {
        return None;
    }
    FRAMES.lock().as_mut()?.alloc()
}

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

/// Physical memory read through the direct map, limited to the ranges the
/// direct map covers. Used while the memory map is still in place.
pub struct DirectMem<'a>(pub &'a BootInfo);

impl kernel_core::acpi::PhysRead for DirectMem<'_> {
    fn read(&self, phys: u64, len: usize) -> Option<&[u8]> {
        let end = phys.checked_add(len as u64)?;
        let mut page = phys / PAGE_SIZE * PAGE_SIZE;
        while page < end {
            let covered = memory_map(self.0).any(|e| {
                let start = e.phys_start;
                let stop = start + e.page_count * PAGE_SIZE;
                crate::space::DIRECT_KINDS
                    .iter()
                    .any(|&k| k as u32 == e.kind)
                    && (start..stop).contains(&page)
            });
            if !covered {
                return None;
            }
            page += PAGE_SIZE;
        }
        // SAFETY: every page of the range is in the kernel's direct map,
        // which maps it readable, and boot memory is not reclaimed while
        // this reader is used.
        Some(unsafe { core::slice::from_raw_parts(crate::space::direct(phys) as *const u8, len) })
    }
}
