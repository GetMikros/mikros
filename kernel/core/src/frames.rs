//! The physical frame allocator: a bitmap with one bit per 4 KiB frame,
//! set while the frame is free.

use crate::sync::SpinLock;
use boot_protocol::{Kind, MemoryMapEntry, PAGE_SIZE};

/// The frame allocator, set up by `init`.
pub static FRAMES: SpinLock<Option<Frames<'static>>> =
    SpinLock::new(crate::lock_order::FRAMES, None);

/// A frame from the frame allocator, or `None` when none is free.
pub fn alloc_frame() -> Option<u64> {
    #[cfg(feature = "fuzz")]
    if crate::fuzz::alloc_fails(crate::fuzz::FRAMES) {
        return None;
    }
    FRAMES.lock().as_mut()?.alloc()
}

/// `count` contiguous frames, aligned to `align` frames and ending at or
/// below the physical address `limit`, as `Frames::alloc_contiguous`
/// takes them, or `None` when there is no such run.
pub fn alloc_contiguous(count: u64, align: u64, limit: u64) -> Option<u64> {
    #[cfg(feature = "fuzz")]
    if crate::fuzz::alloc_fails(crate::fuzz::FRAMES) {
        return None;
    }
    FRAMES
        .lock()
        .as_mut()?
        .alloc_contiguous(count, align, limit)
}

/// The largest alignment of a contiguous run, in frames (2 MiB).
pub const MAX_ALIGN: u64 = 512;

/// The kinds of memory the allocator can hand out: `Usable` from the
/// start, the boot memory the kernel reclaims later, and the boot image,
/// whose frames return when its memory object is destroyed.
fn allocatable(kind: u32) -> bool {
    [
        Kind::Usable,
        Kind::BootloaderReclaimable,
        Kind::PageTables,
        Kind::KernelStack,
        Kind::BootImage,
    ]
    .iter()
    .any(|&k| k as u32 == kind)
}

/// The number of bitmap words needed to cover every range of `memory_map`
/// that the allocator can come to hand out.
pub fn bitmap_words(memory_map: impl IntoIterator<Item = MemoryMapEntry>) -> usize {
    let end = memory_map
        .into_iter()
        .filter(|e| allocatable(e.kind))
        .map(|e| e.phys_start / PAGE_SIZE + e.page_count)
        .max()
        .unwrap_or(0);
    end.div_ceil(64) as usize
}

pub struct Frames<'a> {
    bits: &'a mut [u64],
    free: u64,
    /// The frames the allocator manages, free or in use.
    total: u64,
}

impl<'a> Frames<'a> {
    /// An allocator over `bits` with every frame in use.
    pub fn new(bits: &'a mut [u64]) -> Self {
        bits.fill(0);
        Frames {
            bits,
            free: 0,
            total: 0,
        }
    }

    fn frames(&self) -> u64 {
        self.bits.len() as u64 * 64
    }

    fn is_free(&self, frame: u64) -> bool {
        self.bits[(frame / 64) as usize] & (1 << (frame % 64)) != 0
    }

    fn set(&mut self, frame: u64, free: bool) {
        let (word, bit) = ((frame / 64) as usize, 1 << (frame % 64));
        if free {
            self.bits[word] |= bit;
        } else {
            self.bits[word] &= !bit;
        }
    }

    /// Makes the frames of `count` from `first` free, clipped to the frames
    /// the bitmap covers. Frames already free stay free.
    pub fn add(&mut self, first: u64, count: u64) {
        let end = first.saturating_add(count).min(self.frames());
        for frame in first..end {
            if !self.is_free(frame) {
                self.set(frame, true);
                self.free += 1;
            }
        }
    }

    /// Makes free every frame of the ranges of `kind` in `memory_map`.
    pub fn add_kind(&mut self, memory_map: impl IntoIterator<Item = MemoryMapEntry>, kind: Kind) {
        for e in memory_map.into_iter().filter(|e| e.kind == kind as u32) {
            let before = self.free;
            self.add(e.phys_start / PAGE_SIZE, e.page_count);
            self.total += self.free - before;
        }
    }

    /// Counts `count` frames in use, which come back with `free`, as
    /// managed.
    pub fn adopt(&mut self, count: u64) {
        self.total += count;
    }

    /// The number of frames the allocator manages.
    pub fn total_count(&self) -> u64 {
        self.total
    }

    /// Marks the frames of `count` from `first` as in use.
    pub fn reserve(&mut self, first: u64, count: u64) {
        let end = first.saturating_add(count).min(self.frames());
        for frame in first..end {
            if self.is_free(frame) {
                self.set(frame, false);
                self.free -= 1;
            }
        }
    }

    /// The number of free frames.
    pub fn free_count(&self) -> u64 {
        self.free
    }

    /// Allocates one frame and returns its physical address.
    pub fn alloc(&mut self) -> Option<u64> {
        let word = self.bits.iter().position(|&w| w != 0)?;
        let frame = word as u64 * 64 + u64::from(self.bits[word].trailing_zeros());
        self.set(frame, false);
        self.free -= 1;
        Some(frame * PAGE_SIZE)
    }

    /// Allocates `count` contiguous frames whose first frame number is a
    /// multiple of `align`, a power of two of at most `MAX_ALIGN`, and
    /// that end at or below the physical address `limit`, and returns the
    /// physical address of the first. The lowest such run is taken.
    pub fn alloc_contiguous(&mut self, count: u64, align: u64, limit: u64) -> Option<u64> {
        if count == 0 || !align.is_power_of_two() || align > MAX_ALIGN || count > self.free {
            return None;
        }
        let end = self.frames().min(limit / PAGE_SIZE);
        let mut start = 0;
        while start + count <= end {
            match (start..start + count).find(|&f| !self.is_free(f)) {
                None => {
                    self.reserve(start, count);
                    return Some(start * PAGE_SIZE);
                }
                Some(used) => start = (used + 1).next_multiple_of(align),
            }
        }
        None
    }

    /// Frees `count` frames from the physical address `phys`, which an
    /// allocation returned. In every build, a frame of them that is free
    /// already panics, with its address and the number of frames freed,
    /// rather than counting as free twice.
    pub fn free(&mut self, phys: u64, count: u64) {
        let first = phys / PAGE_SIZE;
        let end = first.saturating_add(count).min(self.frames());
        if let Some(frame) = (first..end).find(|&f| self.is_free(f)) {
            panic!(
                "freeing frame {:#x}, which is free already, in a free of {count} frames from {phys:#x}",
                frame * PAGE_SIZE
            );
        }
        self.add(first, count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn entry(start: u64, pages: u64, kind: Kind) -> MemoryMapEntry {
        MemoryMapEntry {
            phys_start: start * PAGE_SIZE,
            page_count: pages,
            kind: kind as u32,
            reserved: 0,
        }
    }

    fn map() -> Vec<MemoryMapEntry> {
        vec![
            entry(0, 2, Kind::Reserved),
            entry(2, 6, Kind::Usable),
            entry(8, 4, Kind::KernelImage),
            entry(12, 1100, Kind::Usable),
            entry(1112, 4, Kind::Mmio),
        ]
    }

    #[test]
    fn frees_only_usable_frames() {
        let map = map();
        assert_eq!(bitmap_words(map.iter().copied()), 1112usize.div_ceil(64));
        let mut bits = vec![0xffff; bitmap_words(map.iter().copied())];
        let mut frames = Frames::new(&mut bits);
        frames.add_kind(map.iter().copied(), Kind::Usable);
        assert_eq!(frames.free_count(), 1106);
        let mut got = Vec::new();
        while let Some(phys) = frames.alloc() {
            got.push(phys / PAGE_SIZE);
        }
        assert_eq!(got.len(), 1106);
        assert!(
            got.iter()
                .all(|&f| (2..8).contains(&f) || (12..1112).contains(&f))
        );
        assert_eq!(frames.free_count(), 0);
    }

    #[test]
    #[should_panic(
        expected = "freeing frame 0x3000, which is free already, in a free of 2 frames from 0x2000"
    )]
    fn panics_on_a_frame_freed_twice() {
        let mut bits = vec![0; 1];
        let mut frames = Frames::new(&mut bits);
        frames.add(2, 4);
        let first = frames.alloc_contiguous(2, 1, u64::MAX).unwrap();
        assert_eq!(first, 2 * PAGE_SIZE);
        frames.free(first + PAGE_SIZE, 1);
        frames.free(first, 2);
    }

    #[test]
    fn covers_boot_memory_reclaimed_later() {
        let mut map = map();
        map.push(entry(2000, 10, Kind::BootloaderReclaimable));
        let mut bits = vec![0; bitmap_words(map.iter().copied())];
        let mut frames = Frames::new(&mut bits);
        frames.add_kind(map.iter().copied(), Kind::Usable);
        frames.add_kind(map.iter().copied(), Kind::BootloaderReclaimable);
        assert_eq!(frames.free_count(), 1116);
        // Allocating changes what is free, not what is managed.
        frames.alloc().unwrap();
        frames.adopt(3);
        assert_eq!((frames.free_count(), frames.total_count()), (1115, 1119));
    }

    #[test]
    fn aligns_contiguous_runs() {
        let map = map();
        let mut bits = vec![0; bitmap_words(map.iter().copied())];
        let mut frames = Frames::new(&mut bits);
        frames.add_kind(map.iter().copied(), Kind::Usable);
        assert_eq!(frames.alloc_contiguous(4, 4, u64::MAX), Some(4 * PAGE_SIZE));
        assert_eq!(
            frames.alloc_contiguous(10, 8, u64::MAX),
            Some(16 * PAGE_SIZE)
        );
        assert_eq!(
            frames.alloc_contiguous(512, 512, u64::MAX),
            Some(512 * PAGE_SIZE)
        );
        assert_eq!(frames.alloc_contiguous(512, 512, u64::MAX), None);
        assert_eq!(frames.alloc_contiguous(1, 3, u64::MAX), None);
        assert_eq!(frames.alloc_contiguous(1, 1024, u64::MAX), None);
        assert_eq!(frames.alloc_contiguous(0, 1, u64::MAX), None);
        // Frames 2 and 3 are still free, below every other free run.
        assert_eq!(frames.alloc_contiguous(4, 1, 8 * PAGE_SIZE), None);
        assert_eq!(
            frames.alloc_contiguous(2, 1, 4 * PAGE_SIZE),
            Some(2 * PAGE_SIZE)
        );
    }

    #[test]
    fn reuses_freed_frames_and_respects_reservations() {
        let map = [entry(0, 64, Kind::Usable)];
        let mut bits = vec![0; 1];
        let mut frames = Frames::new(&mut bits);
        frames.add_kind(map.iter().copied(), Kind::Usable);
        frames.reserve(0, 3);
        assert_eq!(frames.alloc(), Some(3 * PAGE_SIZE));
        let run = frames.alloc_contiguous(8, 8, u64::MAX).unwrap();
        assert_eq!(run, 8 * PAGE_SIZE);
        frames.free(run, 8);
        assert_eq!(frames.free_count(), 60);
        assert_eq!(frames.alloc_contiguous(8, 8, u64::MAX), Some(8 * PAGE_SIZE));
    }
}
