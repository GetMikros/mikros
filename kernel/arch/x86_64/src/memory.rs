//! Physical memory at boot.

use boot_protocol::{BootInfo, Kind, PAGE_SIZE};
use kernel_core::frames::{self, Frames};

pub use kernel_core::boot::memory_map;
pub use kernel_core::frames::{FRAMES, alloc_contiguous, alloc_frame};

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
