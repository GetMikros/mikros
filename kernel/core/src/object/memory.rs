//! The bookkeeping of a memory object's pages: its size, its options and
//! the frames allocated for it so far.

use crate::nodes::{self, Claim, Map};
use crate::status::Error;
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
/// Physically contiguous RAM, allocated at creation.
pub const KIND_CONTIGUOUS: u32 = 3;
/// RAM allocated at creation and kept at the same physical addresses.
pub const KIND_PINNED: u32 = 4;
/// The kind of memory objects whose pages a pager supplies.
pub const KIND_PAGED: u32 = 5;
/// The creation option of paged objects that enables dirty tracking.
pub const TRACK_DIRTY: u32 = 1 << 1;
/// The creation option of paged objects whose writes to pages being
/// written back wait until the writeback ends. Needs `TRACK_DIRTY`.
pub const STABLE: u32 = 1 << 2;

/// The seals of `mem_seal`.
pub const SEAL_SEAL: u32 = 1 << 0;
pub const SEAL_SHRINK: u32 = 1 << 1;
pub const SEAL_GROW: u32 = 1 << 2;
pub const SEAL_WRITE: u32 = 1 << 3;
pub const SEALS: u32 = SEAL_SEAL | SEAL_SHRINK | SEAL_GROW | SEAL_WRITE;

/// The creation option that allows changing the size.
pub const RESIZABLE: u32 = 1 << 0;

/// The option of `mem_clone` that gives the clone the right to write.
pub const CLONE_WRITE: u32 = 1 << 1;

/// The kernel memory charged for each allocated page: its entry in the
/// tree and that entry's share of the tree's nodes.
pub const PAGE_ENTRY_COST: u64 = 2 * size_of::<(u64, u64)>() as u64;

/// The bit of a recorded frame that says more than one use refers to it,
/// and the bit that says the slot is one of an object made by `mem_clone`
/// and not the frame's owner. Frames are page aligned, so the lowest bits
/// are free.
const SHARED: u64 = 1;
const COPY: u64 = 2;

/// A set of page indexes kept as sorted ranges, start inclusive and end
/// exclusive, that never touch each other.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ranges {
    list: Vec<(u64, u64)>,
}

impl Ranges {
    pub fn new() -> Ranges {
        Ranges { list: Vec::new() }
    }

    /// How many ranges there are.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Makes room for `more` ranges, or fails with `NoMemory`.
    pub fn reserve(&mut self, more: usize) -> Result<(), Error> {
        self.list.try_reserve(more).map_err(|_| Error::NoMemory)
    }

    /// Adds the pages `first` to `end`, exclusive, which lie after those
    /// already in the set, and joins them to the last range when they
    /// touch it. Needs room, which `reserve` makes.
    pub fn push(&mut self, first: u64, end: u64) {
        debug_assert!(first < end && self.list.last().is_none_or(|&(_, last)| last <= first));
        match self.list.last_mut() {
            Some(last) if last.1 == first => last.1 = end,
            _ => self.list.push((first, end)),
        }
    }

    /// The position of the range that holds `index`.
    fn find(&self, index: u64) -> Option<usize> {
        let at = self.list.partition_point(|&(_, end)| end <= index);
        self.list
            .get(at)
            .is_some_and(|&(first, _)| first <= index)
            .then_some(at)
    }

    pub fn contains(&self, index: u64) -> bool {
        self.find(index).is_some()
    }

    /// Whether taking out `index` leaves one more range than there is.
    pub fn splits(&self, index: u64) -> bool {
        self.find(index)
            .is_some_and(|at| self.list[at].0 < index && index + 1 < self.list[at].1)
    }

    /// Whether taking out the pages `first` to `end`, exclusive, leaves one
    /// more range than there is, because they lie inside one.
    pub fn splits_inside(&self, first: u64, end: u64) -> bool {
        self.find(first)
            .is_some_and(|at| self.list[at].0 < first && end < self.list[at].1)
    }

    /// Takes `index` out of the set. Fails with `NoMemory`, changing
    /// nothing, when that splits a range and there is no room.
    pub fn remove(&mut self, index: u64) -> Result<(), Error> {
        self.cut(index, index + 1)
    }

    /// Takes the pages `first` to `end`, exclusive, out of the set. Fails
    /// with `NoMemory`, changing nothing, when that splits a range and
    /// there is no room.
    pub fn cut(&mut self, first: u64, end: u64) -> Result<(), Error> {
        let from = self.list.partition_point(|&(_, e)| e <= first);
        let to = self.list.partition_point(|&(s, _)| s < end);
        if from >= to {
            return Ok(());
        }
        let (head, tail) = (self.list[from], self.list[to - 1]);
        let (keep_head, keep_tail) = (head.0 < first, tail.1 > end);
        if keep_head && keep_tail && from == to - 1 {
            self.reserve(1)?;
            self.list[from] = (head.0, first);
            self.list.insert(from + 1, (end, tail.1));
            return Ok(());
        }
        let mut kept = Vec::new();
        if keep_head {
            kept.push((head.0, first));
        }
        if keep_tail {
            kept.push((end, tail.1));
        }
        self.list.splice(from..to, kept);
        Ok(())
    }

    /// The pages of the set inside `first` to `end`, exclusive, with
    /// `first` taken as 0. Fails with `NoMemory` without room.
    pub fn within(&self, first: u64, end: u64) -> Result<Ranges, Error> {
        let from = self.list.partition_point(|&(_, e)| e <= first);
        let to = self.list.partition_point(|&(s, _)| s < end);
        let mut out = Ranges::new();
        out.reserve(to.saturating_sub(from))?;
        for &(s, e) in &self.list[from.min(to)..to] {
            out.list.push((s.max(first) - first, e.min(end) - first));
        }
        Ok(out)
    }

    /// The ranges, by their first page.
    pub fn iter(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.list.iter().copied()
    }
}

/// A frame that left the pages of an object, with whether it was shared
/// when it left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame(u64);

impl Frame {
    /// A frame that no other use refers to.
    pub fn private(phys: u64) -> Frame {
        debug_assert!(phys & (SHARED | COPY) == 0, "frames are page aligned");
        Frame(phys)
    }

    /// The address of the frame.
    pub fn phys(self) -> u64 {
        self.0 & !(SHARED | COPY)
    }

    /// Whether the slot it came from was one of an object made by
    /// `mem_clone`.
    pub fn is_copy(self) -> bool {
        self.0 & COPY != 0
    }

    /// Whether the slot it came from was marked shared.
    pub fn is_shared(self) -> bool {
        self.0 & SHARED != 0
    }
}

/// The pages of an anonymous memory object.
pub struct Pages {
    size: u64,
    options: u32,
    /// The frame of each page that has one, by page index, with the
    /// `SHARED` bit when the frame has an entry in the table of shared
    /// frames: an unmarked slot's frame is used by no other.
    frames: Map<u64, Frame>,
    /// How many retained writers or readers may still use frames that
    /// were taken from the pages.
    pins: u64,
    /// Frames taken from the pages while pinned, freed once unpinned.
    orphans: Vec<Frame>,
    /// Whether the frames of a shrink are still being removed, during
    /// which the size does not change again.
    trimming: bool,
    /// The pages that are still to be taken from a paged object, which
    /// have no frame.
    pub(crate) pending: Option<crate::memory_object::Pending>,
}

/// What changing the size leaves to the caller.
pub struct Resized {
    /// The frame whose bytes from this offset to its end must be zeroed,
    /// so that bytes past the smaller of the two sizes read as zero.
    pub zero: Option<(u64, usize)>,
    /// The frames of the pages past the new size, by page index, which
    /// left the pages. While the pages are pinned, the caller keeps them
    /// with `orphan`; otherwise it frees them.
    pub removed: Map<u64, Frame>,
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
            frames: nodes::map(),
            pins: 0,
            orphans: Vec::new(),
            trimming: false,
            pending: None,
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

    /// Whether `test` holds for the frame of some page.
    pub fn any_frame(&self, test: impl Fn(u64) -> bool) -> bool {
        self.frames.values().any(|frame| test(frame.phys()))
    }

    /// The frame of page `index`, if it has one.
    pub fn frame(&self, index: u64) -> Option<u64> {
        self.frames.get(&index).map(|frame| frame.phys())
    }

    /// Whether page `index` has a frame that is marked shared.
    pub fn is_shared(&self, index: u64) -> bool {
        self.frames
            .get(&index)
            .is_some_and(|frame| frame.is_shared())
    }

    /// Whether page `index` has a frame that its object got from a
    /// `mem_clone`, not as the owner of it.
    pub fn is_copy(&self, index: u64) -> bool {
        self.frames.get(&index).is_some_and(|frame| frame.is_copy())
    }

    /// Records `phys`, a frame that is already shared, as the frame of
    /// page `index` of an object made by `mem_clone`, as `insert` does.
    pub fn insert_copy(&mut self, index: u64, phys: u64, claim: Claim) {
        self.insert_shared(index, phys, claim);
        if let Some(slot) = self.frames.get_mut(&index) {
            slot.0 |= COPY;
        }
    }

    /// Marks the frame of page `index`, which has one, as shared.
    pub fn mark_shared(&mut self, index: u64) {
        let slot = self.frames.get_mut(&index);
        debug_assert!(slot.is_some(), "page {index} has no frame");
        if let Some(slot) = slot {
            slot.0 |= SHARED;
        }
    }

    /// Records `phys` as the frame of page `index`, which is inside the
    /// object and has none, using the room of `claim`.
    pub fn insert(&mut self, index: u64, phys: u64, claim: Claim) {
        debug_assert!(index < self.count());
        let old = self.frames.insert(index, Frame::private(phys));
        debug_assert!(old.is_none(), "page {index} already has a frame");
        drop(claim);
    }

    /// Records `phys`, a frame that is already shared, as the frame of
    /// page `index`, as `insert` does.
    pub fn insert_shared(&mut self, index: u64, phys: u64, claim: Claim) {
        self.insert(index, phys, claim);
        self.mark_shared(index);
    }

    /// Makes `phys` the frame of page `index`, which has one, marked
    /// shared or not, and returns the frame it had. It needs no room.
    pub fn replace(&mut self, index: u64, phys: u64, shared: bool) -> Frame {
        debug_assert!(phys & (SHARED | COPY) == 0, "frames are page aligned");
        let slot = self
            .frames
            .get_mut(&index)
            .expect("the page of a replaced frame has one");
        let old = *slot;
        *slot = Frame(phys | if shared { SHARED } else { 0 });
        old
    }

    /// Takes every frame, by page index. Nothing may be pinned.
    pub fn take_all(&mut self) -> Map<u64, Frame> {
        debug_assert!(self.pins == 0 && self.orphans.is_empty());
        core::mem::replace(&mut self.frames, nodes::map())
    }

    /// Whether the frames of a shrink are still being removed.
    pub fn trimming(&self) -> bool {
        self.trimming
    }

    /// Marks the frames of a shrink as being removed, or as removed.
    pub fn set_trimming(&mut self, trimming: bool) {
        self.trimming = trimming;
    }

    /// Keeps `frame`, which left the pages, until the last `unpin`, or
    /// fails with `NoMemory` without room for it.
    pub fn orphan(&mut self, frame: Frame) -> Result<(), Error> {
        debug_assert!(self.pins > 0);
        self.orphans.try_reserve(1).map_err(|_| Error::NoMemory)?;
        self.orphans.push(frame);
        Ok(())
    }

    /// Makes room for one more `orphan`, or fails with `NoMemory`, so that
    /// a frame can leave the pages and be kept without failing.
    pub fn reserve_orphan(&mut self) -> Result<(), Error> {
        if self.pins == 0 {
            return Ok(());
        }
        self.orphans.try_reserve(1).map_err(|_| Error::NoMemory)
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
    pub fn unpin(&mut self) -> Vec<Frame> {
        debug_assert!(self.pins > 0, "unpinned more than pinned");
        self.pins -= 1;
        if self.pins == 0 {
            core::mem::take(&mut self.orphans)
        } else {
            Vec::new()
        }
    }

    /// Changes the size to `size`, at most `max`. Fails with
    /// `NotSupported` unless the object was created `RESIZABLE`, and with
    /// `NoMemory`, changing nothing, without room to track the change.
    pub fn set_size(&mut self, size: u64, max: u64) -> Result<Resized, Error> {
        if self.options & RESIZABLE == 0 {
            return Err(Error::NotSupported);
        }
        check_size(size, max)?;
        // Splitting the map allocates at most one insertion's nodes.
        let _claim = nodes::claim_insert()?;
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

    /// Takes the frames of pages `first` to `end`, exclusive, which then
    /// read as zero, and returns those to free now: while the pages are
    /// pinned they are kept until `unpin` instead. Fails with `NoMemory`,
    /// taking nothing, without room for the frames. The frames keep their
    /// charges until they are freed, so those cover the room.
    pub fn decommit(&mut self, first: u64, end: u64) -> Result<Vec<Frame>, Error> {
        let count = self.frames.range(first..end).count();
        let mut taken = Vec::new();
        let list = if self.pins > 0 {
            &mut self.orphans
        } else {
            &mut taken
        };
        list.try_reserve(count).map_err(|_| Error::NoMemory)?;
        list.extend(
            self.frames
                .extract_if(first..end, |_, _| true)
                .map(|(_, frame)| frame),
        );
        Ok(taken)
    }

    /// The memory information record of an object of `kind` created with
    /// `options`, which include those of the pages, with `seals`.
    pub fn info(&self, kind: u32, options: u32, seals: u32) -> [u8; INFO_MEMORY_SIZE] {
        debug_assert_eq!(options & self.options, self.options);
        let mut record = [0u8; INFO_MEMORY_SIZE];
        record[0x00..0x08].copy_from_slice(&self.size.to_le_bytes());
        record[0x08..0x0C].copy_from_slice(&kind.to_le_bytes());
        record[0x0C..0x10].copy_from_slice(&options.to_le_bytes());
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
    fn ranges_hold_pages_and_split_when_one_is_taken() {
        let mut ranges = Ranges::new();
        ranges.reserve(4).unwrap();
        ranges.push(2, 5);
        ranges.push(5, 6);
        ranges.push(9, 12);
        assert_eq!(ranges.iter().collect::<Vec<_>>(), [(2, 6), (9, 12)]);
        assert!(ranges.contains(2) && ranges.contains(5) && !ranges.contains(6));
        assert!(!ranges.contains(1) && !ranges.contains(8) && ranges.contains(11));
        assert!(ranges.splits(3) && !ranges.splits(2) && !ranges.splits(5) && !ranges.splits(7));
        ranges.remove(3).unwrap();
        assert_eq!(ranges.iter().collect::<Vec<_>>(), [(2, 3), (4, 6), (9, 12)]);
        ranges.remove(2).unwrap();
        ranges.remove(11).unwrap();
        assert_eq!(ranges.iter().collect::<Vec<_>>(), [(4, 6), (9, 11)]);
        ranges.cut(5, 10).unwrap();
        assert_eq!(ranges.iter().collect::<Vec<_>>(), [(4, 5), (10, 11)]);
        ranges.cut(0, 100).unwrap();
        assert!(ranges.is_empty());
    }

    #[test]
    fn ranges_cut_the_middle_of_one_and_rebase_what_lies_inside() {
        let mut ranges = Ranges::new();
        ranges.reserve(4).unwrap();
        ranges.push(10, 20);
        ranges.push(30, 40);
        ranges.cut(12, 15).unwrap();
        assert_eq!(
            ranges.iter().collect::<Vec<_>>(),
            [(10, 12), (15, 20), (30, 40)]
        );
        let inside = ranges.within(11, 35).unwrap();
        assert_eq!(
            inside.iter().collect::<Vec<_>>(),
            [(0, 1), (4, 9), (19, 24)]
        );
        assert!(ranges.within(20, 30).unwrap().is_empty());
        assert!(ranges.within(50, 60).unwrap().is_empty());
    }

    #[test]
    fn checks_ranges() {
        let pages = Pages::new(100, 0, MAX).unwrap();
        assert!(pages.check_range(0, 100).is_ok());
        assert!(pages.check_range(100, 0).is_ok());
        assert_eq!(pages.check_range(99, 2), Err(Error::OutOfRange));
        assert_eq!(pages.check_range(1, u64::MAX), Err(Error::OutOfRange));
    }

    fn put(pages: &mut Pages, index: u64, phys: u64) {
        pages.insert(index, phys, nodes::claim_insert().unwrap());
    }

    fn physs(frames: impl IntoIterator<Item = Frame>) -> Vec<u64> {
        frames.into_iter().map(Frame::phys).collect()
    }

    #[test]
    fn decommits_ranges() {
        let mut pages = Pages::new(4 * PAGE_SIZE, 0, MAX).unwrap();
        for i in 0..4 {
            put(&mut pages, i, 0x1000 * (i + 1));
        }
        assert_eq!(physs(pages.decommit(1, 3).unwrap()), [0x2000, 0x3000]);
        assert_eq!(
            (pages.frame(0), pages.frame(1), pages.frame(3)),
            (Some(0x1000), None, Some(0x4000))
        );
        pages.pin();
        assert!(pages.decommit(0, 4).unwrap().is_empty());
        assert_eq!(pages.allocated(), 0);
        assert_eq!(physs(pages.unpin()), [0x1000, 0x4000]);
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
            put(&mut pages, i, 0x1000 * (i + 1));
        }
        let resized = pages.set_size(PAGE_SIZE + 10, MAX).unwrap();
        assert_eq!(resized.zero, Some((0x2000, 10)));
        assert_eq!(
            resized
                .removed
                .iter()
                .map(|(&index, frame)| (index, frame.phys()))
                .collect::<std::vec::Vec<_>>(),
            [(2, 0x3000), (3, 0x4000)]
        );
        assert_eq!((pages.count(), pages.allocated()), (2, 2));
    }

    #[test]
    fn keeps_frames_taken_while_pinned_until_the_last_unpin() {
        let mut pages = Pages::new(4 * PAGE_SIZE, RESIZABLE, MAX).unwrap();
        for i in 0..4 {
            put(&mut pages, i, 0x1000 * (i + 1));
        }
        pages.pin();
        pages.pin();
        let shrink = |pages: &mut Pages, size| {
            let removed = pages.set_size(size, MAX).unwrap().removed;
            let indices: Vec<u64> = removed.keys().copied().collect();
            // As the object does: frames leaving while pinned are kept.
            let freed: Vec<u64> = if pages.pinned() {
                for &frame in removed.values() {
                    pages.orphan(frame).unwrap();
                }
                Vec::new()
            } else {
                physs(removed.into_values())
            };
            (indices, freed)
        };
        // The pages that left are reported even while pinned.
        assert_eq!(shrink(&mut pages, 3 * PAGE_SIZE), (vec![3], vec![]));
        pages.set_size(4 * PAGE_SIZE, MAX).unwrap();
        put(&mut pages, 3, 0x9000);
        assert_eq!(shrink(&mut pages, PAGE_SIZE), (vec![1, 2, 3], vec![]));
        assert_eq!(pages.allocated(), 1);
        assert!(pages.unpin().is_empty());
        assert_eq!(physs(pages.unpin()), [0x4000, 0x2000, 0x3000, 0x9000]);
        pages.set_size(2 * PAGE_SIZE, MAX).unwrap();
        put(&mut pages, 1, 0xa000);
        assert_eq!(shrink(&mut pages, PAGE_SIZE), (vec![1], vec![0xa000]));
        assert_eq!((pages.pins, pages.orphans.len()), (0, 0));
    }

    #[test]
    fn marks_frames_as_shared() {
        let mut pages = Pages::new(4 * PAGE_SIZE, RESIZABLE, MAX).unwrap();
        put(&mut pages, 0, 0x1000);
        pages.insert_shared(1, 0x2000, nodes::claim_insert().unwrap());
        put(&mut pages, 2, 0x3000);
        // The mark does not change the address.
        assert_eq!(
            (pages.frame(0), pages.frame(1)),
            (Some(0x1000), Some(0x2000))
        );
        assert!(!pages.is_shared(0) && pages.is_shared(1) && !pages.is_shared(3));
        pages.mark_shared(0);
        assert!(pages.is_shared(0) && pages.frame(0) == Some(0x1000));
        assert!(pages.any_frame(|phys| phys == 0x2000));
        // A replaced frame returns the old one with its mark.
        let old = pages.replace(1, 0x7000, false);
        assert_eq!((old.phys(), old.is_shared()), (0x2000, true));
        assert_eq!((pages.frame(1), pages.is_shared(1)), (Some(0x7000), false));
        let old = pages.replace(2, 0x8000, true);
        assert_eq!((old.phys(), old.is_shared()), (0x3000, false));
        assert!(pages.is_shared(2));
        // Frames leave with the mark they had, taken out or shrunk away.
        let taken = pages.decommit(0, 1).unwrap();
        assert_eq!((taken[0].phys(), taken[0].is_shared()), (0x1000, true));
        let resized = pages.set_size(2 * PAGE_SIZE, MAX).unwrap();
        let left = resized.removed[&2];
        assert_eq!((left.phys(), left.is_shared()), (0x8000, true));
        pages.pin();
        assert_eq!(pages.decommit(1, 2).unwrap().len(), 0);
        pages.orphan(left).unwrap();
        let kept = pages.unpin();
        assert_eq!(kept.len(), 2);
        assert!(!kept[0].is_shared() && kept[1].is_shared());
        let all = {
            put(&mut pages, 0, 0x4000);
            pages.mark_shared(0);
            pages.take_all()
        };
        assert!(all[&0].is_shared() && all[&0].phys() == 0x4000);
    }

    #[test]
    fn zeroes_the_old_tail_when_growing() {
        let mut pages = Pages::new(10, RESIZABLE, MAX).unwrap();
        let resized = pages.set_size(PAGE_SIZE * 3, MAX).unwrap();
        assert!(resized.zero.is_none() && resized.removed.is_empty());
        put(&mut pages, 0, 0x5000);
        pages.set_size(10, MAX).unwrap();
        let resized = pages.set_size(PAGE_SIZE, MAX).unwrap();
        assert_eq!(resized.zero, Some((0x5000, 10)));
        assert!(pages.set_size(PAGE_SIZE * 2, MAX).unwrap().zero.is_none());
    }

    #[test]
    fn writes_the_information_record() {
        let pages = Pages::new(0x1234, RESIZABLE, MAX).unwrap();
        let r = pages.info(KIND_ANONYMOUS, pages.options(), SEAL_GROW);
        assert_eq!(&r[0..8], &0x1234u64.to_le_bytes());
        assert_eq!(&r[8..24], &[1, 0, 0, 0, 1, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0]);
        let r = pages.info(KIND_PAGED, RESIZABLE | TRACK_DIRTY, 0);
        assert_eq!(&r[8..16], &[5, 0, 0, 0, 3, 0, 0, 0]);
    }
}
