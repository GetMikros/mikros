//! Allocating ranges of an address space.

use core::fmt;
use core::ptr::NonNull;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The range overlaps a range that is already free, or lies outside
    /// the managed space.
    NotAllocated { start: u64, size: u64 },
    /// Freeing the range needs room for another free range, and there is
    /// no memory for more.
    NoMemory,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotAllocated { start, size } => {
                write!(
                    f,
                    "the range of {size:#x} bytes at {start:#x} is not allocated"
                )
            }
            Error::NoMemory => write!(f, "no memory for more free ranges"),
        }
    }
}

/// Room for free ranges, each its start and end.
pub type Slots = NonNull<(u64, u64)>;

/// Where a [`Ranges`] keeps its free ranges once they are more than fit
/// in it: room that it takes, twice as large each time, as it needs.
pub trait Room {
    /// What gives room back.
    type Handle: Copy;
    /// Room for `count` free ranges, or `None` without memory.
    fn take(count: usize) -> Option<(Slots, Self::Handle)>;
    /// Gives back the room for `count` free ranges that `take` gave with
    /// `handle`.
    ///
    /// # Safety
    ///
    /// Nothing may use the room any more.
    unsafe fn give(handle: Self::Handle, count: usize);
}

/// How many free ranges fit in a [`Ranges`] itself.
pub const INLINE: usize = 64;

/// The free ranges of the space `[start, end)`, kept sorted and merged
/// with their neighbours: in the ranges themselves while they fit, and
/// then in room from `R`, so that there can be as many as the space has
/// holes while there is memory.
pub struct Ranges<R: Room> {
    start: u64,
    end: u64,
    /// Start and end of each free range, while they fit.
    inline: [(u64, u64); INLINE],
    /// The room taken once they no longer fit.
    room: Option<Taken<R::Handle>>,
    len: usize,
}

/// Room that a [`Ranges`] took, its handle and how many ranges fit in it.
struct Taken<H> {
    slots: Slots,
    handle: H,
    count: usize,
}

// SAFETY: the room belongs to the ranges alone.
unsafe impl<R: Room> Send for Ranges<R> where R::Handle: Send {}

impl<R: Room> Ranges<R> {
    /// A space whose addresses are all free.
    pub const fn new(start: u64, end: u64) -> Self {
        let mut inline = [(0, 0); INLINE];
        let len = if start < end {
            inline[0] = (start, end);
            1
        } else {
            0
        };
        Ranges {
            start,
            end,
            inline,
            room: None,
            len,
        }
    }

    /// A space whose addresses are all free but those from `hole_start` to
    /// `hole_end`, which are never allocated.
    pub const fn with_hole(start: u64, end: u64, hole_start: u64, hole_end: u64) -> Self {
        let mut inline = [(0, 0); INLINE];
        let mut len = 0;
        if start < hole_start && hole_start <= end {
            inline[len] = (start, hole_start);
            len += 1;
        }
        if start <= hole_end && hole_end < end {
            inline[len] = (hole_end, end);
            len += 1;
        }
        Ranges {
            start,
            end,
            inline,
            room: None,
            len,
        }
    }

    /// Where the free ranges are, with room for as many as fit.
    fn slots(&self) -> &[(u64, u64)] {
        let Some(taken) = &self.room else {
            return &self.inline;
        };
        // SAFETY: the room holds `count` ranges, and only these ranges use
        // it.
        unsafe { core::slice::from_raw_parts(taken.slots.as_ptr(), taken.count) }
    }

    fn slots_mut(&mut self) -> &mut [(u64, u64)] {
        let Some(taken) = &self.room else {
            return &mut self.inline;
        };
        // SAFETY: as in `slots`.
        unsafe { core::slice::from_raw_parts_mut(taken.slots.as_ptr(), taken.count) }
    }

    /// Makes room for one more free range if none is left: false when
    /// there is no memory for it.
    fn make_room(&mut self) -> bool {
        let count = self.slots().len();
        if self.len < count {
            return true;
        }
        let Some((room, handle)) = R::take(2 * count) else {
            return false;
        };
        // SAFETY: the new room holds twice as many ranges as the old, and
        // nothing else uses it.
        unsafe { core::ptr::copy_nonoverlapping(self.slots().as_ptr(), room.as_ptr(), self.len) };
        let taken = Taken {
            slots: room,
            handle,
            count: 2 * count,
        };
        if let Some(old) = self.room.replace(taken) {
            // SAFETY: the ranges moved out of the old room.
            unsafe { R::give(old.handle, old.count) };
        }
        true
    }

    /// Allocates `size` bytes starting at a multiple of `align`, a power of
    /// two, from the lowest free range that has room.
    pub fn alloc(&mut self, size: u64, align: u64) -> Option<u64> {
        if size == 0 || !align.is_power_of_two() {
            return None;
        }
        for i in 0..self.len {
            let (start, end) = self.slots()[i];
            let Some(at) = start.checked_next_multiple_of(align) else {
                continue;
            };
            if at.checked_add(size).is_none_or(|e| e > end) {
                continue;
            }
            let after = (at + size, end);
            match (at > start, after.0 < end) {
                (false, false) => self.remove(i),
                (false, true) => self.slots_mut()[i] = after,
                (true, false) => self.slots_mut()[i].1 = at,
                (true, true) => {
                    if !self.make_room() {
                        // No room to split; try another range.
                        continue;
                    }
                    self.slots_mut()[i].1 = at;
                    self.insert(i + 1, after);
                }
            }
            return Some(at);
        }
        None
    }

    /// Returns `size` bytes at `start` to the free ranges.
    pub fn free(&mut self, start: u64, size: u64) -> Result<(), Error> {
        let bad = Error::NotAllocated { start, size };
        let end = start.checked_add(size).ok_or(bad)?;
        if size == 0 || start < self.start || end > self.end {
            return Err(bad);
        }
        let free = &self.slots()[..self.len];
        // The first free range that starts at or after `end`.
        let i = free.partition_point(|&(s, _)| s < end);
        if i > 0 && free[i - 1].1 > start {
            return Err(bad);
        }
        let joins_left = i > 0 && free[i - 1].1 == start;
        let joins_right = i < self.len && free[i].0 == end;
        match (joins_left, joins_right) {
            (true, true) => {
                let right = self.slots()[i].1;
                self.slots_mut()[i - 1].1 = right;
                self.remove(i);
            }
            (true, false) => self.slots_mut()[i - 1].1 = end,
            (false, true) => self.slots_mut()[i].0 = start,
            (false, false) => {
                if !self.make_room() {
                    return Err(Error::NoMemory);
                }
                self.insert(i, (start, end));
            }
        }
        Ok(())
    }

    /// The number of free ranges.
    pub fn count(&self) -> usize {
        self.len
    }

    /// In the diagnostic build, the bytes of the free ranges.
    #[cfg(feature = "diag")]
    pub fn free_bytes(&self) -> u64 {
        self.slots()[..self.len].iter().map(|(s, e)| e - s).sum()
    }

    fn remove(&mut self, i: usize) {
        let len = self.len;
        self.slots_mut().copy_within(i + 1..len, i);
        self.len -= 1;
    }

    /// Inserts `range` at `i`, where `make_room` made room.
    fn insert(&mut self, i: usize, range: (u64, u64)) {
        let len = self.len;
        let slots = self.slots_mut();
        slots.copy_within(i..len, i + 1);
        slots[i] = range;
        self.len += 1;
    }
}

impl<R: Room> Drop for Ranges<R> {
    fn drop(&mut self) {
        if let Some(taken) = self.room.take() {
            // SAFETY: the ranges go, and nothing else uses their room.
            unsafe { R::give(taken.handle, taken.count) };
        }
    }
}

/// Room for free ranges in runs of frames from the frame allocator, which
/// the kernel reaches without mapping them: for the ranges of the kernel's
/// dynamic region, where the heap maps its pages.
pub struct FrameRoom;

impl FrameRoom {
    /// The frames of room for `count` free ranges.
    fn frames(count: usize) -> u64 {
        (count * size_of::<(u64, u64)>()).div_ceil(crate::heap::PAGE_SIZE) as u64
    }
}

impl Room for FrameRoom {
    /// The run's physical address.
    type Handle = u64;

    fn take(count: usize) -> Option<(Slots, u64)> {
        let frames = FrameRoom::frames(count);
        let phys = crate::frames::FRAMES
            .lock()
            .as_mut()?
            .alloc_contiguous(frames, 1, u64::MAX)?;
        let room = NonNull::new(crate::arch::frame_address(phys) as *mut (u64, u64))?;
        Some((room, phys))
    }

    unsafe fn give(phys: u64, count: usize) {
        if let Some(frames) = crate::frames::FRAMES.lock().as_mut() {
            frames.free(phys, FrameRoom::frames(count));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::Layout;
    use std::cell::Cell;

    std::thread_local! {
        static NO_MEMORY: Cell<bool> = const { Cell::new(false) };
    }

    /// Room on the heap, which this thread can make run out.
    struct HeapRoom;

    impl Room for HeapRoom {
        type Handle = Slots;

        fn take(count: usize) -> Option<(Slots, Self::Handle)> {
            if NO_MEMORY.get() {
                return None;
            }
            let layout = Layout::array::<(u64, u64)>(count).ok()?;
            // SAFETY: the layout is not empty.
            let room = NonNull::new(unsafe { std::alloc::alloc(layout) }.cast())?;
            Some((room, room))
        }

        unsafe fn give(room: Self::Handle, count: usize) {
            let layout = Layout::array::<(u64, u64)>(count).unwrap();
            // SAFETY: the room came from the heap with this layout.
            unsafe { std::alloc::dealloc(room.as_ptr().cast(), layout) };
        }
    }

    type Ranges = super::Ranges<HeapRoom>;

    #[test]
    fn allocates_aligned_ranges_and_merges_on_free() {
        let mut r = Ranges::new(0x1000, 0x10_0000);
        assert_eq!(r.alloc(0x2000, 0x1000), Some(0x1000));
        assert_eq!(r.alloc(0x1000, 0x1_0000), Some(0x1_0000));
        // The gap below the aligned range stays free and is used first.
        assert_eq!(r.count(), 2);
        assert_eq!(r.alloc(0x1000, 0x1000), Some(0x3000));
        r.free(0x1000, 0x2000).unwrap();
        r.free(0x1_0000, 0x1000).unwrap();
        r.free(0x3000, 0x1000).unwrap();
        assert_eq!(r.count(), 1);
        assert_eq!(r.alloc(0xF_F000, 0x1000), Some(0x1000));
        assert_eq!(r.alloc(0x1000, 0x1000), None);
    }

    #[test]
    fn never_allocates_the_hole() {
        let mut r = Ranges::with_hole(0x1000, 0x10_0000, 0x4000, 0x8000);
        assert_eq!(r.count(), 2);
        assert_eq!(r.alloc(0x4000, 0x1000), Some(0x8000));
        assert_eq!(r.alloc(0x3000, 0x1000), Some(0x1000));
        assert_eq!(r.alloc(0x1000, 0x1000), Some(0xC000));
        r.free(0x8000, 0x4000).unwrap();
        r.free(0x1000, 0x3000).unwrap();
        r.free(0xC000, 0x1000).unwrap();
        assert_eq!(r.count(), 2);
        // Room only across the hole.
        assert_eq!(r.alloc(0xF_9000, 0x1000), None);
        assert_eq!(r.alloc(0xF_8000, 0x1000), Some(0x8000));
        // A hole at either end leaves one range.
        assert_eq!(Ranges::with_hole(0, 0x10, 0, 0x8).count(), 1);
        assert_eq!(Ranges::with_hole(0, 0x10, 0x8, 0x10).count(), 1);
    }

    #[test]
    fn refuses_bad_frees() {
        let mut r = Ranges::new(0x1000, 0x10_0000);
        let a = r.alloc(0x4000, 0x1000).unwrap();
        let bad = |start, size| Err(Error::NotAllocated { start, size });
        assert_eq!(r.free(a + 0x3000, 0x2000), bad(a + 0x3000, 0x2000));
        assert_eq!(r.free(0, 0x1000), bad(0, 0x1000));
        assert_eq!(r.free(a, 0), bad(a, 0));
        assert_eq!(r.free(u64::MAX, 2), bad(u64::MAX, 2));
        r.free(a, 0x4000).unwrap();
        assert_eq!(r.free(a, 0x1000), bad(a, 0x1000));
    }

    /// Far more holes than the ranges have room for in themselves: every
    /// free is kept, and they all merge again.
    #[test]
    fn keeps_as_many_free_ranges_as_there_are_holes() {
        const PAGES: u64 = 4096;
        let mut r = Ranges::new(0, PAGES * 0x1000);
        let pages: Vec<u64> = (0..PAGES)
            .map(|_| r.alloc(0x1000, 0x1000).unwrap())
            .collect();
        for &page in pages.iter().step_by(2) {
            r.free(page, 0x1000).unwrap();
        }
        assert_eq!(r.count(), PAGES as usize / 2);
        // The lowest hole is used first.
        assert_eq!(r.alloc(0x1000, 0x1000), Some(0));
        r.free(0, 0x1000).unwrap();
        for &page in pages.iter().skip(1).step_by(2) {
            r.free(page, 0x1000).unwrap();
        }
        assert_eq!(r.count(), 1);
        assert_eq!(r.alloc(PAGES * 0x1000, 0x1000), Some(0));
    }

    /// Without memory for more room, a free that needs another range
    /// fails and a split is skipped, as when there was no more room.
    #[test]
    fn needs_memory_only_for_more_room() {
        let mut r = Ranges::new(0, 0x100_0000);
        assert_eq!(r.alloc(0x100_0000, 1), Some(0));
        // Free ranges of three pages, each after a page in use.
        for i in 0..INLINE as u64 {
            r.free(i * 0x4000 + 0x1000, 0x3000).unwrap();
        }
        NO_MEMORY.set(true);
        let next = INLINE as u64 * 0x4000;
        assert_eq!(r.free(next + 0x2000, 0x1000), Err(Error::NoMemory));
        // Freeing next to a free range needs no more room.
        r.free(next, 0x1000).unwrap();
        assert_eq!(r.count(), INLINE);
        // Each range has an aligned page only in its middle, which splits
        // it.
        assert_eq!(r.alloc(0x1000, 0x2000), None);
        NO_MEMORY.set(false);
        assert_eq!(r.alloc(0x1000, 0x2000), Some(0x2000));
        assert_eq!(r.count(), INLINE + 1);
        r.free(next + 0x2000, 0x1000).unwrap();
        assert_eq!(r.count(), INLINE + 2);
    }
}
