//! The kernel heap: size classes carved from pages, and whole pages for
//! larger blocks.

use core::alloc::Layout;
use core::ptr::NonNull;

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
}

/// A free block, which holds the next free block of its class.
struct Free {
    next: Option<NonNull<Free>>,
}

pub struct Heap<P: Pages> {
    pages: P,
    free: [Option<NonNull<Free>>; CLASSES],
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
        }
    }

    /// A block for `layout`, or `None` when no memory is left. Blocks
    /// aligned to more than a page are not supported.
    pub fn alloc(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let Some(c) = class(layout) else {
            if layout.align() > PAGE_SIZE {
                return None;
            }
            return self.pages.alloc(pages_for(layout));
        };
        if self.free[c].is_none() {
            self.refill(c)?;
        }
        let block = self.free[c]?;
        // SAFETY: blocks on a free list are unused, belong to the heap
        // and hold a `Free`.
        self.free[c] = unsafe { block.as_ref().next };
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
                let block = block.cast::<Free>();
                // SAFETY: the caller gives the block back, so the heap may
                // write to it; it is aligned for `Free`.
                unsafe { block.write(Free { next: self.free[c] }) };
                self.free[c] = Some(block);
            }
            None => self.pages.free(block, pages_for(layout)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{alloc, dealloc};
    use std::vec::Vec;

    /// Pages from the host allocator, counted.
    #[derive(Default)]
    struct HostPages {
        live: Vec<(usize, usize)>,
        limit: usize,
    }

    impl Pages for HostPages {
        fn alloc(&mut self, count: usize) -> Option<NonNull<u8>> {
            let used: usize = self.live.iter().map(|l| l.1).sum();
            if used + count > self.limit {
                return None;
            }
            let layout = Layout::from_size_align(count * PAGE_SIZE, PAGE_SIZE).unwrap();
            // SAFETY: the layout has a nonzero size.
            let p = NonNull::new(unsafe { alloc(layout) })?;
            self.live.push((p.as_ptr() as usize, count));
            Some(p)
        }
        fn free(&mut self, start: NonNull<u8>, count: usize) {
            let i = self
                .live
                .iter()
                .position(|&l| l == (start.as_ptr() as usize, count))
                .expect("freeing pages that were not allocated");
            self.live.remove(i);
            let layout = Layout::from_size_align(count * PAGE_SIZE, PAGE_SIZE).unwrap();
            // SAFETY: the pages came from `alloc` with this layout.
            unsafe { dealloc(start.as_ptr(), layout) };
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
}
