//! The kernel heap: size classes carved from pages, and whole pages for
//! larger blocks.

use core::alloc::Layout;
use core::ptr::NonNull;

#[cfg(feature = "diag")]
mod check;
#[cfg(feature = "diag")]
pub use check::Fault;

pub const PAGE_SIZE: usize = 4096;
/// The smallest block size.
const MIN_CLASS: usize = 16;
/// The largest block size served from a size class.
pub const MAX_CLASS: usize = 2048;
const CLASSES: usize = (MAX_CLASS / MIN_CLASS).trailing_zeros() as usize + 1;

/// Where the heap gets its pages.
pub trait Pages {
    /// `count` contiguous, writable pages starting at a page boundary.
    fn alloc(&mut self, count: usize) -> Option<NonNull<u8>>;
    /// Returns `count` pages from `alloc`.
    fn free(&mut self, start: NonNull<u8>, count: usize);
    /// In the diagnostic build, the records of the heap check, one for
    /// each page from the returned address on. Pages past them go
    /// unchecked.
    #[cfg(feature = "diag")]
    fn records(&mut self) -> (usize, &mut [u32]);
}

/// A free block, which holds the next free block of its class.
struct Free {
    next: Option<NonNull<Free>>,
}

pub struct Heap<P: Pages> {
    pages: P,
    free: [Option<NonNull<Free>>; CLASSES],
    /// In the diagnostic build, the failed check that made the last
    /// `alloc` or `free` stop, for the caller to report once it no
    /// longer holds the heap's lock.
    #[cfg(feature = "diag")]
    pub fault: Option<Fault>,
}

// SAFETY: the heap owns the blocks on its free lists; moving it to
// another thread moves that ownership with it.
unsafe impl<P: Pages + Send> Send for Heap<P> {}

/// The size class index for `layout`, or `None` when it needs whole pages.
fn class(layout: Layout) -> Option<usize> {
    let size = layout.size().max(layout.align()).max(MIN_CLASS);
    (size <= MAX_CLASS).then(|| (size.next_power_of_two() / MIN_CLASS).trailing_zeros() as usize)
}

fn pages_for(layout: Layout) -> usize {
    layout.size().max(1).div_ceil(PAGE_SIZE)
}

impl<P: Pages> Heap<P> {
    pub const fn new(pages: P) -> Self {
        Heap {
            pages,
            free: [None; CLASSES],
            #[cfg(feature = "diag")]
            fault: None,
        }
    }

    /// A block for `layout`, or `None` when no memory is left. Blocks
    /// aligned to more than a page are not supported.
    pub fn alloc(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let Some(c) = class(layout) else {
            if layout.align() > PAGE_SIZE {
                return None;
            }
            let start = self.pages.alloc(pages_for(layout))?;
            #[cfg(feature = "diag")]
            check::pages_taken(&mut self.pages, start, pages_for(layout));
            return Some(start);
        };
        if self.free[c].is_none() {
            self.refill(c)?;
        }
        let block = self.free[c]?;
        // SAFETY: a block recorded with its class is the heap's.
        #[cfg(feature = "diag")]
        if let Err(fault) = unsafe { check::taking(&mut self.pages, block.cast(), c) } {
            self.fault = Some(fault);
            return None;
        }
        // SAFETY: blocks on a free list are unused, belong to the heap
        // and hold a `Free`.
        self.free[c] = unsafe { block.as_ref().next };
        // SAFETY: the block was checked and is now in use.
        #[cfg(feature = "diag")]
        unsafe {
            check::taken(block.cast())
        };
        Some(block.cast())
    }

    /// Splits a new page into blocks of class `c`.
    fn refill(&mut self, c: usize) -> Option<()> {
        let page = self.pages.alloc(1)?;
        let size = MIN_CLASS << c;
        for offset in (0..PAGE_SIZE).step_by(size).rev() {
            // SAFETY: the block lies inside the new page, is aligned to
            // its size, which is at least that of `Free`, and is unused.
            unsafe {
                let block = page.add(offset).cast::<Free>();
                block.write(Free { next: self.free[c] });
                self.free[c] = Some(block);
            }
        }
        // SAFETY: the page is the heap's, split into free blocks of `c`.
        #[cfg(feature = "diag")]
        unsafe {
            check::refilled(&mut self.pages, page, c)
        };
        Some(())
    }

    /// Returns a block from `alloc` with the same `layout`.
    ///
    /// # Safety
    ///
    /// `block` must come from `alloc` on this heap with `layout`, and must
    /// not be used afterwards.
    pub unsafe fn free(&mut self, block: NonNull<u8>, layout: Layout) {
        match class(layout) {
            Some(c) => {
                // SAFETY: a block recorded with its class is the heap's.
                #[cfg(feature = "diag")]
                if let Err(fault) =
                    unsafe { check::freeing(&mut self.pages, block, c, layout.size()) }
                {
                    self.fault = Some(fault);
                    return;
                }
                let block = block.cast::<Free>();
                // SAFETY: the caller gives the block back, so the heap may
                // write to it; it is aligned for `Free`.
                unsafe { block.write(Free { next: self.free[c] }) };
                self.free[c] = Some(block);
                // SAFETY: the block was checked and is now free.
                #[cfg(feature = "diag")]
                unsafe {
                    check::freed(&mut self.pages, block.cast(), c)
                };
            }
            None => {
                #[cfg(feature = "diag")]
                if let Err(fault) = check::pages_freed(&mut self.pages, block, pages_for(layout)) {
                    self.fault = Some(fault);
                    return;
                }
                self.pages.free(block, pages_for(layout));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::alloc;
    #[cfg(not(feature = "diag"))]
    use std::alloc::dealloc;
    use std::vec::Vec;

    /// Pages from the host allocator, counted.
    #[derive(Default)]
    struct HostPages {
        live: Vec<(usize, usize)>,
        limit: usize,
        /// The start of the pages the heap check's tests use, and their
        /// records.
        #[cfg(feature = "diag")]
        base: usize,
        #[cfg(feature = "diag")]
        records: Vec<u32>,
    }

    /// The pages that the heap check's tests take their pages from.
    #[cfg(feature = "diag")]
    const ARENA_PAGES: usize = 16;

    impl Pages for HostPages {
        fn alloc(&mut self, count: usize) -> Option<NonNull<u8>> {
            let used: usize = self.live.iter().map(|l| l.1).sum();
            if used + count > self.limit {
                return None;
            }
            #[cfg(feature = "diag")]
            {
                // Pages come from one range, which the records cover.
                let base = self.arena();
                let at = (0..=ARENA_PAGES - count).find(|&i| {
                    let (s, e) = (base + i * PAGE_SIZE, base + (i + count) * PAGE_SIZE);
                    self.live
                        .iter()
                        .all(|&(l, n)| l + n * PAGE_SIZE <= s || e <= l)
                })?;
                self.live.push((base + at * PAGE_SIZE, count));
                return NonNull::new((base + at * PAGE_SIZE) as *mut u8);
            }
            #[cfg(not(feature = "diag"))]
            {
                let layout = Layout::from_size_align(count * PAGE_SIZE, PAGE_SIZE).unwrap();
                // SAFETY: the layout has a nonzero size.
                let p = NonNull::new(unsafe { alloc(layout) })?;
                self.live.push((p.as_ptr() as usize, count));
                Some(p)
            }
        }
        fn free(&mut self, start: NonNull<u8>, count: usize) {
            let i = self
                .live
                .iter()
                .position(|&l| l == (start.as_ptr() as usize, count))
                .expect("freeing pages that were not allocated");
            self.live.remove(i);
            #[cfg(not(feature = "diag"))]
            {
                let layout = Layout::from_size_align(count * PAGE_SIZE, PAGE_SIZE).unwrap();
                // SAFETY: the pages came from `alloc` with this layout.
                unsafe { dealloc(start.as_ptr(), layout) };
            }
        }

        #[cfg(feature = "diag")]
        fn records(&mut self) -> (usize, &mut [u32]) {
            let base = self.arena();
            (base, &mut self.records)
        }
    }

    #[cfg(feature = "diag")]
    impl HostPages {
        /// The start of the pages that this heap check's test takes its
        /// pages from, leaked.
        fn arena(&mut self) -> usize {
            if self.base == 0 {
                let layout = Layout::from_size_align(ARENA_PAGES * PAGE_SIZE, PAGE_SIZE).unwrap();
                // SAFETY: the layout has a nonzero size.
                self.base = unsafe { alloc(layout) } as usize;
                self.records = std::vec![0; ARENA_PAGES];
            }
            self.base
        }
    }

    fn heap(limit: usize) -> Heap<HostPages> {
        Heap::new(HostPages {
            limit,
            ..HostPages::default()
        })
    }

    #[test]
    fn picks_size_classes() {
        let l = |size, align| Layout::from_size_align(size, align).unwrap();
        assert_eq!(class(l(1, 1)), Some(0));
        assert_eq!(class(l(17, 8)), Some(1));
        assert_eq!(class(l(8, 512)), Some(5));
        assert_eq!(class(l(2048, 8)), Some(7));
        assert_eq!(class(l(2049, 8)), None);
        assert_eq!(pages_for(l(4097, 8)), 2);
    }

    #[test]
    fn reuses_blocks_and_keeps_them_apart() {
        let mut h = heap(4);
        let l = Layout::from_size_align(24, 8).unwrap();
        let blocks: Vec<_> = (0..PAGE_SIZE / 32).map(|_| h.alloc(l).unwrap()).collect();
        assert_eq!(h.pages.live.len(), 1);
        let mut addresses: Vec<usize> = blocks.iter().map(|b| b.as_ptr() as usize).collect();
        addresses.sort();
        assert!(addresses.windows(2).all(|w| w[1] - w[0] >= 32));
        assert!(addresses.iter().all(|a| a % 32 == 0));
        let first = blocks[0];
        // SAFETY: each block came from this heap with `l`.
        unsafe { h.free(first, l) };
        assert_eq!(h.alloc(l), Some(first));
        // The class is exhausted, so a new page is taken.
        h.alloc(l).unwrap();
        assert_eq!(h.pages.live.len(), 2);
    }

    #[test]
    fn serves_large_blocks_from_pages() {
        let mut h = heap(3);
        let big = Layout::from_size_align(5000, 4096).unwrap();
        let b = h.alloc(big).unwrap();
        assert_eq!(b.as_ptr() as usize % PAGE_SIZE, 0);
        assert_eq!(h.alloc(big), None);
        // SAFETY: the block came from this heap with `big`.
        unsafe { h.free(b, big) };
        assert!(h.pages.live.is_empty());
        assert_eq!(h.alloc(Layout::from_size_align(8, 8192).unwrap()), None);
    }

    #[cfg(feature = "diag")]
    mod check {
        use super::*;

        fn small() -> Layout {
            Layout::from_size_align(40, 8).unwrap()
        }

        #[test]
        fn finds_a_double_free() {
            let mut h = heap(4);
            let b = h.alloc(small()).unwrap();
            // SAFETY: the block came from this heap with this layout; the
            // second free is the fault under test, which the check stops.
            unsafe {
                h.free(b, small());
                h.free(b, small());
            }
            let f = h.fault.take().unwrap();
            assert_eq!(
                (f.what, f.block),
                ("freeing a block twice", b.as_ptr() as usize)
            );
            assert_eq!(h.alloc(small()), Some(b));
            assert_eq!(h.fault, None);
        }

        #[test]
        fn finds_a_write_after_free() {
            let mut h = heap(4);
            let b = h.alloc(small()).unwrap();
            // SAFETY: as above; the write after the free is the fault
            // under test.
            unsafe {
                h.free(b, small());
                b.cast::<u64>().add(3).write(7);
            }
            assert_eq!(h.alloc(small()), None);
            let f = h.fault.take().unwrap();
            assert_eq!(f.what, "a freed block was written after its free");
            assert_eq!((f.block, f.offset, f.found), (b.as_ptr() as usize, 24, 7));
            assert!(f.to_string().contains("0x7 at offset 24"));
        }

        #[test]
        fn finds_frees_with_another_size() {
            let mut h = heap(4);
            let b = h.alloc(small()).unwrap();
            // SAFETY: the free with the wrong layout is the fault under
            // test, which the check stops.
            unsafe { h.free(b, Layout::from_size_align(100, 8).unwrap()) };
            assert!(
                h.fault
                    .take()
                    .unwrap()
                    .what
                    .starts_with("freeing a block that")
            );
            let big = Layout::from_size_align(2 * PAGE_SIZE, 8).unwrap();
            let p = h.alloc(big).unwrap();
            // SAFETY: as above.
            unsafe { h.free(p, Layout::from_size_align(PAGE_SIZE, 8).unwrap()) };
            assert!(
                h.fault
                    .take()
                    .unwrap()
                    .what
                    .starts_with("freeing whole pages")
            );
            // SAFETY: the block came from this heap with `big`.
            unsafe { h.free(p, big) };
            assert_eq!(h.fault, None);
        }

        #[test]
        fn finds_a_damaged_free_list() {
            let mut h = heap(4);
            let a = h.alloc(small()).unwrap();
            let big = h
                .alloc(Layout::from_size_align(PAGE_SIZE, 8).unwrap())
                .unwrap();
            // SAFETY: the block came from this heap with this layout; the
            // link written after its free is the fault under test.
            unsafe {
                h.free(a, small());
                a.cast::<usize>().write(big.as_ptr() as usize);
            }
            assert_eq!(h.alloc(small()), Some(a));
            assert_eq!(h.alloc(small()), None);
            let f = h.fault.take().unwrap();
            assert_eq!(f.what, "a free list leads to a block that is not free");
            assert_eq!(f.block, big.as_ptr() as usize);
        }
    }
}
