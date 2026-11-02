//! Allocating ranges of an address space.

use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The range overlaps a range that is already free, or lies outside
    /// the managed space.
    NotAllocated { start: u64, size: u64 },
    /// Freeing the range would need more free ranges than there is room
    /// for.
    Full,
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
            Error::Full => write!(f, "too many free ranges"),
        }
    }
}

/// The free ranges of the space `[start, end)`, at most `N` of them, kept
/// sorted and merged with their neighbours.
pub struct Ranges<const N: usize> {
    start: u64,
    end: u64,
    /// Start and end of each free range.
    free: [(u64, u64); N],
    len: usize,
}

impl<const N: usize> Ranges<N> {
    /// A space whose addresses are all free.
    pub const fn new(start: u64, end: u64) -> Self {
        let mut free = [(0, 0); N];
        let len = if start < end && N > 0 {
            free[0] = (start, end);
            1
        } else {
            0
        };
        Ranges {
            start,
            end,
            free,
            len,
        }
    }

    /// Allocates `size` bytes starting at a multiple of `align`, a power of
    /// two, from the lowest free range that has room.
    pub fn alloc(&mut self, size: u64, align: u64) -> Option<u64> {
        if size == 0 || !align.is_power_of_two() {
            return None;
        }
        for i in 0..self.len {
            let (start, end) = self.free[i];
            let Some(at) = start.checked_next_multiple_of(align) else {
                continue;
            };
            if at.checked_add(size).is_none_or(|e| e > end) {
                continue;
            }
            let after = (at + size, end);
            match (at > start, after.0 < end) {
                (false, false) => self.remove(i),
                (false, true) => self.free[i] = after,
                (true, false) => self.free[i].1 = at,
                (true, true) => {
                    if self.len == N {
                        // No room to split; try another range.
                        continue;
                    }
                    self.free[i].1 = at;
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
        // The first free range that starts at or after `end`.
        let i = self.free[..self.len].partition_point(|&(s, _)| s < end);
        if i > 0 && self.free[i - 1].1 > start {
            return Err(bad);
        }
        let joins_left = i > 0 && self.free[i - 1].1 == start;
        let joins_right = i < self.len && self.free[i].0 == end;
        match (joins_left, joins_right) {
            (true, true) => {
                self.free[i - 1].1 = self.free[i].1;
                self.remove(i);
            }
            (true, false) => self.free[i - 1].1 = end,
            (false, true) => self.free[i].0 = start,
            (false, false) => {
                if self.len == N {
                    return Err(Error::Full);
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

    fn remove(&mut self, i: usize) {
        self.free.copy_within(i + 1..self.len, i);
        self.len -= 1;
    }

    fn insert(&mut self, i: usize, range: (u64, u64)) {
        self.free.copy_within(i..self.len, i + 1);
        self.free[i] = range;
        self.len += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_aligned_ranges_and_merges_on_free() {
        let mut r = Ranges::<4>::new(0x1000, 0x10_0000);
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
    fn refuses_bad_frees() {
        let mut r = Ranges::<4>::new(0x1000, 0x10_0000);
        let a = r.alloc(0x4000, 0x1000).unwrap();
        let bad = |start, size| Err(Error::NotAllocated { start, size });
        assert_eq!(r.free(a + 0x3000, 0x2000), bad(a + 0x3000, 0x2000));
        assert_eq!(r.free(0, 0x1000), bad(0, 0x1000));
        assert_eq!(r.free(a, 0), bad(a, 0));
        assert_eq!(r.free(u64::MAX, 2), bad(u64::MAX, 2));
        r.free(a, 0x4000).unwrap();
        assert_eq!(r.free(a, 0x1000), bad(a, 0x1000));
    }

    #[test]
    fn reports_when_full() {
        let mut r = Ranges::<2>::new(0, 0x10_000);
        let all = r.alloc(0x10_000, 1).unwrap();
        r.free(all, 0x1000).unwrap();
        r.free(all + 0x2000, 0x1000).unwrap();
        assert_eq!(r.free(all + 0x4000, 0x1000), Err(Error::Full));
        // Freeing next to an existing range needs no new slot.
        r.free(all + 0x1000, 0x1000).unwrap();
        assert_eq!(r.count(), 1);
        // A split that needs a slot is skipped when there is none.
        let mut r = Ranges::<1>::new(0, 0x10_000);
        assert_eq!(r.alloc(0x1000, 0x2000), Some(0));
        assert_eq!(r.alloc(0x1000, 0x4000), None);
        assert_eq!(r.alloc(0x1000, 0x1000), Some(0x1000));
    }
}
