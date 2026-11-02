//! The physical frame allocator: a bitmap with one bit per 4 KiB frame,
//! set while the frame is free.

use boot_protocol::{Kind, MemoryMapEntry, PAGE_SIZE};

/// The largest alignment of a contiguous run, in frames (2 MiB).
pub const MAX_ALIGN: u64 = 512;

/// The number of bitmap words needed to cover every `Usable` range of
/// `memory_map`.
pub fn bitmap_words(memory_map: impl IntoIterator<Item = MemoryMapEntry>) -> usize {
    let end = memory_map
        .into_iter()
        .filter(|e| e.kind == Kind::Usable as u32)
        .map(|e| e.phys_start / PAGE_SIZE + e.page_count)
        .max()
        .unwrap_or(0);
    end.div_ceil(64) as usize
}

pub struct Frames<'a> {
    bits: &'a mut [u64],
    free: u64,
}

impl<'a> Frames<'a> {
    /// An allocator over `bits` with every frame in use.
    pub fn new(bits: &'a mut [u64]) -> Self {
        bits.fill(0);
        Frames { bits, free: 0 }
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

    /// Makes free every frame of the `Usable` ranges of `memory_map`.
    pub fn add_usable(&mut self, memory_map: impl IntoIterator<Item = MemoryMapEntry>) {
        for e in memory_map
            .into_iter()
            .filter(|e| e.kind == Kind::Usable as u32)
        {
            self.add(e.phys_start / PAGE_SIZE, e.page_count);
        }
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
    /// returns the physical address of the first.
    pub fn alloc_contiguous(&mut self, count: u64, align: u64) -> Option<u64> {
        if count == 0 || !align.is_power_of_two() || align > MAX_ALIGN || count > self.free {
            return None;
        }
        let mut start = 0;
        while start + count <= self.frames() {
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
    /// allocation returned.
    pub fn free(&mut self, phys: u64, count: u64) {
        let first = phys / PAGE_SIZE;
        debug_assert!(
            (first..first + count).all(|f| !self.is_free(f)),
            "freeing a free frame"
        );
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
        ]
    }

    #[test]
    fn frees_only_usable_frames() {
        let map = map();
        assert_eq!(bitmap_words(map.iter().copied()), 1112usize.div_ceil(64));
        let mut bits = vec![0xffff; bitmap_words(map.iter().copied())];
        let mut frames = Frames::new(&mut bits);
        frames.add_usable(map.iter().copied());
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
    fn aligns_contiguous_runs() {
        let map = map();
        let mut bits = vec![0; bitmap_words(map.iter().copied())];
        let mut frames = Frames::new(&mut bits);
        frames.add_usable(map.iter().copied());
        assert_eq!(frames.alloc_contiguous(4, 4), Some(4 * PAGE_SIZE));
        assert_eq!(frames.alloc_contiguous(10, 8), Some(16 * PAGE_SIZE));
        assert_eq!(frames.alloc_contiguous(512, 512), Some(512 * PAGE_SIZE));
        assert_eq!(frames.alloc_contiguous(512, 512), None);
        assert_eq!(frames.alloc_contiguous(1, 3), None);
        assert_eq!(frames.alloc_contiguous(1, 1024), None);
        assert_eq!(frames.alloc_contiguous(0, 1), None);
    }

    #[test]
    fn reuses_freed_frames_and_respects_reservations() {
        let map = [entry(0, 64, Kind::Usable)];
        let mut bits = vec![0; 1];
        let mut frames = Frames::new(&mut bits);
        frames.add_usable(map.iter().copied());
        frames.reserve(0, 3);
        assert_eq!(frames.alloc(), Some(3 * PAGE_SIZE));
        let run = frames.alloc_contiguous(8, 8).unwrap();
        assert_eq!(run, 8 * PAGE_SIZE);
        frames.free(run, 8);
        assert_eq!(frames.free_count(), 60);
        assert_eq!(frames.alloc_contiguous(8, 8), Some(8 * PAGE_SIZE));
    }
}
