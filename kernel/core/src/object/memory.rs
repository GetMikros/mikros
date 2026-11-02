//! The bookkeeping of a memory object's pages: its size, its options and
//! the frames allocated for it so far.

use crate::status::Error;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

pub const PAGE_SIZE: u64 = 4096;

/// The topic of the memory information record.
pub const INFO_MEMORY: u32 = 4;
/// The size of the memory information record.
pub const INFO_MEMORY_SIZE: usize = 0x18;

/// The kind of anonymous memory objects.
pub const KIND_ANONYMOUS: u32 = 1;
/// The kind of memory objects of a fixed physical range.
pub const KIND_PHYSICAL: u32 = 2;
/// The kind of memory objects whose pages a pager supplies.
pub const KIND_PAGED: u32 = 5;
/// The creation option of paged objects that enables dirty tracking.
pub const TRACK_DIRTY: u32 = 1 << 1;

/// The seals of `mem_seal`.
pub const SEAL_SEAL: u32 = 1 << 0;
pub const SEAL_SHRINK: u32 = 1 << 1;
pub const SEAL_GROW: u32 = 1 << 2;
pub const SEAL_WRITE: u32 = 1 << 3;
pub const SEALS: u32 = SEAL_SEAL | SEAL_SHRINK | SEAL_GROW | SEAL_WRITE;

/// The creation option that allows changing the size.
pub const RESIZABLE: u32 = 1 << 0;

/// The kernel memory charged for each allocated page: its entry in the
/// tree and that entry's share of the tree's nodes.
pub const PAGE_ENTRY_COST: u64 = 2 * size_of::<(u64, u64)>() as u64;

/// The pages of an anonymous memory object.
pub struct Pages {
    size: u64,
    options: u32,
    /// The frame of each page that has one, by page index.
    frames: BTreeMap<u64, u64>,
    /// How many retained writers or readers may still use frames that
    /// were taken from the pages.
    pins: u64,
    /// Frames taken from the pages while pinned, freed once unpinned.
    orphans: Vec<u64>,
}

/// What changing the size leaves to the caller.
pub struct Resized {
    /// The frame whose bytes from this offset to its end must be zeroed,
    /// so that bytes past the smaller of the two sizes read as zero.
    pub zero: Option<(u64, usize)>,
    /// The frames of the pages past the new size, by page index, which
    /// left the pages. The caller passes them to `discard`.
    pub removed: BTreeMap<u64, u64>,
}

/// Fails with `OutOfRange` unless `size` is not zero and at most `max`.
fn check_size(size: u64, max: u64) -> Result<(), Error> {
    if size == 0 || size > max {
        return Err(Error::OutOfRange);
    }
    Ok(())
}

impl Pages {
    /// The pages of an object of `size` bytes created with `options`,
    /// where `max` is the largest size allowed.
    pub fn new(size: u64, options: u32, max: u64) -> Result<Pages, Error> {
        if options & !RESIZABLE != 0 {
            return Err(Error::InvalidArgs);
        }
        check_size(size, max)?;
        Ok(Pages {
            size,
            options,
            frames: BTreeMap::new(),
            pins: 0,
            orphans: Vec::new(),
        })
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn options(&self) -> u32 {
        self.options
    }

    /// The number of pages the object covers.
    pub fn count(&self) -> u64 {
        self.size.div_ceil(PAGE_SIZE)
    }

    /// The number of pages that have a frame.
    pub fn allocated(&self) -> u64 {
        self.frames.len() as u64
    }

    /// Fails with `OutOfRange` unless the `len` bytes from `offset` lie
    /// inside the object.
    pub fn check_range(&self, offset: u64, len: u64) -> Result<(), Error> {
        match offset.checked_add(len) {
            Some(end) if end <= self.size => Ok(()),
            _ => Err(Error::OutOfRange),
        }
    }

    /// The frame of page `index`, if it has one.
    pub fn frame(&self, index: u64) -> Option<u64> {
        self.frames.get(&index).copied()
    }

    /// Records `phys` as the frame of page `index`, which is inside the
    /// object and has none.
    pub fn insert(&mut self, index: u64, phys: u64) {
        debug_assert!(index < self.count());
        let old = self.frames.insert(index, phys);
        debug_assert!(old.is_none(), "page {index} already has a frame");
    }

    /// Takes every frame, by page index. Nothing may be pinned.
    pub fn take_all(&mut self) -> BTreeMap<u64, u64> {
        debug_assert!(self.pins == 0 && self.orphans.is_empty());
        core::mem::take(&mut self.frames)
    }

    /// Whether a `pin` is in effect.
    pub fn pinned(&self) -> bool {
        self.pins > 0
    }

    /// Keeps frames that later leave the pages from being returned until
    /// the matching `unpin`.
    pub fn pin(&mut self) {
        self.pins += 1;
    }

    /// Ends one `pin`, and returns the frames to free once none is left.
    pub fn unpin(&mut self) -> Vec<u64> {
        debug_assert!(self.pins > 0, "unpinned more than pinned");
        self.pins -= 1;
        if self.pins == 0 {
            core::mem::take(&mut self.orphans)
        } else {
            Vec::new()
        }
    }

    /// Changes the size to `size`, at most `max`. Fails with
    /// `NotSupported` unless the object was created `RESIZABLE`.
    pub fn set_size(&mut self, size: u64, max: u64) -> Result<Resized, Error> {
        if self.options & RESIZABLE == 0 {
            return Err(Error::NotSupported);
        }
        check_size(size, max)?;
        let kept = self.size.min(size);
        let tail = (kept % PAGE_SIZE) as usize;
        let zero = if tail == 0 {
            None
        } else {
            self.frame(kept / PAGE_SIZE).map(|phys| (phys, tail))
        };
        self.size = size;
        let removed = self.frames.split_off(&self.count());
        Ok(Resized { zero, removed })
    }

    /// Returns those of `frames`, which left the pages, to free now: while
    /// the pages are pinned they are kept until `unpin` instead.
    pub fn discard(&mut self, frames: impl IntoIterator<Item = u64>) -> Vec<u64> {
        if self.pins > 0 {
            self.orphans.extend(frames);
            return Vec::new();
        }
        frames.into_iter().collect()
    }

    /// Takes the frames of pages `first` to `end`, exclusive, which then
    /// read as zero, and returns those to free now: while the pages are
    /// pinned they are kept until `unpin` instead.
    pub fn decommit(&mut self, first: u64, end: u64) -> Vec<u64> {
        let mut tail = self.frames.split_off(&first);
        let mut after = tail.split_off(&end);
        self.frames.append(&mut after);
        let taken: Vec<u64> = tail.into_values().collect();
        if self.pins > 0 {
            self.orphans.extend(taken);
            return Vec::new();
        }
        taken
    }

    /// The memory information record of an object of `kind` with
    /// `seals`.
    pub fn info(&self, kind: u32, seals: u32) -> [u8; INFO_MEMORY_SIZE] {
        let mut record = [0u8; INFO_MEMORY_SIZE];
        record[0x00..0x08].copy_from_slice(&self.size.to_le_bytes());
        record[0x08..0x0C].copy_from_slice(&kind.to_le_bytes());
        record[0x0C..0x10].copy_from_slice(&self.options.to_le_bytes());
        record[0x10..0x14].copy_from_slice(&seals.to_le_bytes());
        record
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: u64 = 1 << 47;

    #[test]
    fn checks_size_and_options() {
        assert_eq!(Pages::new(0, 0, MAX).err(), Some(Error::OutOfRange));
        assert_eq!(Pages::new(MAX + 1, 0, MAX).err(), Some(Error::OutOfRange));
        assert_eq!(Pages::new(1, 2, MAX).err(), Some(Error::InvalidArgs));
        let pages = Pages::new(MAX, RESIZABLE, MAX).unwrap();
        assert_eq!(pages.count(), MAX / PAGE_SIZE);
        assert_eq!(Pages::new(4097, 0, MAX).unwrap().count(), 2);
    }

    #[test]
    fn checks_ranges() {
        let pages = Pages::new(100, 0, MAX).unwrap();
        assert!(pages.check_range(0, 100).is_ok());
        assert!(pages.check_range(100, 0).is_ok());
        assert_eq!(pages.check_range(99, 2), Err(Error::OutOfRange));
        assert_eq!(pages.check_range(1, u64::MAX), Err(Error::OutOfRange));
    }

    #[test]
    fn decommits_ranges() {
        let mut pages = Pages::new(4 * PAGE_SIZE, 0, MAX).unwrap();
        for i in 0..4 {
            pages.insert(i, 0x1000 * (i + 1));
        }
        assert_eq!(pages.decommit(1, 3), [0x2000, 0x3000]);
        assert_eq!(
            (pages.frame(0), pages.frame(1), pages.frame(3)),
            (Some(0x1000), None, Some(0x4000))
        );
        pages.pin();
        assert!(pages.decommit(0, 4).is_empty());
        assert_eq!(pages.allocated(), 0);
        assert_eq!(pages.unpin(), [0x1000, 0x4000]);
    }

    #[test]
    fn resizes_only_resizable_objects() {
        let mut pages = Pages::new(100, 0, MAX).unwrap();
        assert_eq!(pages.set_size(200, MAX).err(), Some(Error::NotSupported));
        let mut pages = Pages::new(100, RESIZABLE, MAX).unwrap();
        assert_eq!(pages.set_size(0, MAX).err(), Some(Error::OutOfRange));
        assert_eq!(pages.size(), 100);
    }

    #[test]
    fn frees_pages_past_a_smaller_size() {
        let mut pages = Pages::new(4 * PAGE_SIZE, RESIZABLE, MAX).unwrap();
        for i in 0..4 {
            pages.insert(i, 0x1000 * (i + 1));
        }
        let resized = pages.set_size(PAGE_SIZE + 10, MAX).unwrap();
        assert_eq!(resized.zero, Some((0x2000, 10)));
        assert_eq!(
            resized
                .removed
                .clone()
                .into_iter()
                .collect::<std::vec::Vec<_>>(),
            [(2, 0x3000), (3, 0x4000)]
        );
        assert_eq!(
            pages.discard(resized.removed.into_values()),
            [0x3000, 0x4000]
        );
        assert_eq!((pages.count(), pages.allocated()), (2, 2));
    }

    #[test]
    fn keeps_frames_taken_while_pinned_until_the_last_unpin() {
        let mut pages = Pages::new(4 * PAGE_SIZE, RESIZABLE, MAX).unwrap();
        for i in 0..4 {
            pages.insert(i, 0x1000 * (i + 1));
        }
        pages.pin();
        pages.pin();
        let shrink = |pages: &mut Pages, size| {
            let removed = pages.set_size(size, MAX).unwrap().removed;
            let indices: Vec<u64> = removed.keys().copied().collect();
            (indices, pages.discard(removed.into_values()))
        };
        // The pages that left are reported even while pinned.
        assert_eq!(shrink(&mut pages, 3 * PAGE_SIZE), (vec![3], vec![]));
        pages.set_size(4 * PAGE_SIZE, MAX).unwrap();
        pages.insert(3, 0x9000);
        assert_eq!(shrink(&mut pages, PAGE_SIZE), (vec![1, 2, 3], vec![]));
        assert_eq!(pages.allocated(), 1);
        assert!(pages.unpin().is_empty());
        assert_eq!(pages.unpin(), [0x4000, 0x2000, 0x3000, 0x9000]);
        pages.set_size(2 * PAGE_SIZE, MAX).unwrap();
        pages.insert(1, 0xa000);
        assert_eq!(shrink(&mut pages, PAGE_SIZE), (vec![1], vec![0xa000]));
        assert_eq!((pages.pins, pages.orphans.len()), (0, 0));
    }

    #[test]
    fn zeroes_the_old_tail_when_growing() {
        let mut pages = Pages::new(10, RESIZABLE, MAX).unwrap();
        let resized = pages.set_size(PAGE_SIZE * 3, MAX).unwrap();
        assert!(resized.zero.is_none() && resized.removed.is_empty());
        pages.insert(0, 0x5000);
        pages.set_size(10, MAX).unwrap();
        let resized = pages.set_size(PAGE_SIZE, MAX).unwrap();
        assert_eq!(resized.zero, Some((0x5000, 10)));
        assert!(pages.set_size(PAGE_SIZE * 2, MAX).unwrap().zero.is_none());
    }

    #[test]
    fn writes_the_information_record() {
        let pages = Pages::new(0x1234, RESIZABLE, MAX).unwrap();
        let r = pages.info(KIND_ANONYMOUS, SEAL_GROW);
        assert_eq!(&r[0..8], &0x1234u64.to_le_bytes());
        assert_eq!(&r[8..24], &[1, 0, 0, 0, 1, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0]);
    }
}
