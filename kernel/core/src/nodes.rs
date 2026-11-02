//! The allocator of the kernel's ordered maps and sets, which can take
//! nodes from blocks claimed beforehand when the heap has no room, so that
//! an insertion whose claim succeeded does not fail.
//!
//! Every block starts with a header that says where it came from. Heap
//! blocks are the node's size plus the header; reserve blocks are all
//! `BLOCK` bytes.

use crate::status::Error;
use crate::sync::SpinLock;
use alloc::collections::{BTreeMap, BTreeSet};
use core::alloc::{AllocError, Allocator, AllocatorClone, Layout};
use core::ptr::NonNull;

/// The allocator of the kernel's maps and sets.
#[derive(Clone, Copy, Debug, Default)]
pub struct Nodes;

/// An ordered map whose nodes come from [`Nodes`].
pub type Map<K, V> = BTreeMap<K, V, Nodes>;
/// An ordered set whose nodes come from [`Nodes`].
pub type Set<T> = BTreeSet<T, Nodes>;

/// An empty map.
pub const fn map<K, V>() -> Map<K, V> {
    BTreeMap::new_in(Nodes)
}

/// An empty set.
pub const fn set<T>() -> Set<T> {
    BTreeSet::new_in(Nodes)
}

/// The size of a reserve block, which holds the header and the largest
/// node of any map or set the kernel keeps.
pub const BLOCK: usize = 1024;

/// The size and alignment of the header in front of every node.
const HEADER: usize = 16;

/// The most nodes one insertion allocates: one for each level of a tree
/// of up to `INSERT - 1` levels, which holds more entries than memory
/// can, and a new root.
pub const INSERT: usize = 12;

/// Header values.
const FROM_HEAP: u64 = 0;
const FROM_RESERVE: u64 = 1;

/// Free reserve blocks are linked through their first word.
struct Free {
    next: Option<NonNull<Free>>,
}

struct Reserve {
    free: Option<NonNull<Free>>,
    /// The number of free blocks.
    count: usize,
    /// The number of blocks promised to live claims, including those
    /// they already took.
    claimed: usize,
}

// SAFETY: the free blocks belong to the reserve alone.
unsafe impl Send for Reserve {}

static RESERVE: SpinLock<Reserve> = SpinLock::new(Reserve {
    free: None,
    count: 0,
    claimed: 0,
});

const fn block_layout() -> Layout {
    match Layout::from_size_align(BLOCK, HEADER) {
        Ok(layout) => layout,
        Err(_) => panic!("invalid block layout"),
    }
}

/// The layout of the heap block for a node of `layout`.
fn heap_layout(layout: Layout) -> Option<Layout> {
    Layout::from_size_align(layout.size().checked_add(HEADER)?, HEADER).ok()
}

/// A block from the heap, or `None` if it has no room.
fn heap_alloc(layout: Layout) -> Option<NonNull<u8>> {
    #[cfg(test)]
    if tests::HEAP_FULL.with(|full| full.get()) {
        return None;
    }
    // SAFETY: the layout's size is not zero.
    NonNull::new(unsafe { alloc::alloc::alloc(layout) })
}

impl Reserve {
    fn push(&mut self, block: NonNull<u8>) {
        let free = block.cast::<Free>();
        // SAFETY: the block is at least as large and aligned as `Free`,
        // and nothing else uses it.
        unsafe { free.write(Free { next: self.free }) };
        self.free = Some(free);
        self.count += 1;
    }

    fn pop(&mut self) -> Option<NonNull<u8>> {
        let free = self.free?;
        // SAFETY: free blocks hold a `Free`.
        self.free = unsafe { free.read().next };
        self.count -= 1;
        Some(free.cast())
    }
}

/// Blocks set aside for `count` nodes, which insertions may take while
/// the claim lives when the heap has no room. Ending the claim gives
/// back the blocks it did not use.
#[must_use]
pub struct Claim {
    count: usize,
}

/// Claims blocks for `count` nodes, taking them from the heap if the
/// reserve does not have them, or fails with `NoMemory`.
pub fn claim(count: usize) -> Result<Claim, Error> {
    #[cfg(test)]
    if tests::CLAIMS_FAIL.with(|fail| fail.get()) {
        return Err(Error::NoMemory);
    }
    loop {
        let missing = {
            let mut reserve = RESERVE.lock();
            // Blocks a claim used stay claimed until it ends, so that
            // claims never count on blocks another claim may still take.
            let available = reserve.count.saturating_sub(reserve.claimed);
            if available >= count {
                reserve.claimed += count;
                return Ok(Claim { count });
            }
            count - available
        };
        for _ in 0..missing {
            let block = heap_alloc(block_layout()).ok_or(Error::NoMemory)?;
            RESERVE.lock().push(block);
        }
    }
}

/// Claims blocks for one insertion.
pub fn claim_insert() -> Result<Claim, Error> {
    claim(INSERT)
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut surplus = None;
        {
            let mut reserve = RESERVE.lock();
            reserve.claimed = reserve.claimed.saturating_sub(self.count);
            // One insertion's worth stays for the next claim.
            while reserve.count > reserve.claimed + INSERT {
                let block = reserve.pop().expect("the reserve has blocks");
                // SAFETY: the block is free.
                unsafe { block.cast::<Free>().write(Free { next: surplus }) };
                surplus = Some(block.cast::<Free>());
            }
        }
        while let Some(free) = surplus {
            // SAFETY: the list holds the surplus blocks.
            surplus = unsafe { free.read().next };
            // SAFETY: reserve blocks come from the heap with this layout.
            unsafe { alloc::alloc::dealloc(free.as_ptr().cast(), block_layout()) };
        }
    }
}

// SAFETY: blocks are valid for the layout until deallocated, and `Nodes`
// values are interchangeable, since they share the heap and the reserve.
unsafe impl Allocator for Nodes {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.align() > HEADER {
            return Err(AllocError);
        }
        let heap = heap_layout(layout).ok_or(AllocError)?;
        let (block, origin) = match heap_alloc(heap) {
            Some(block) => (block, FROM_HEAP),
            None if heap.size() <= BLOCK => {
                let block = RESERVE.lock().pop().ok_or(AllocError)?;
                (block, FROM_RESERVE)
            }
            None => return Err(AllocError),
        };
        // SAFETY: the block starts with room for the header.
        unsafe { block.cast::<u64>().write(origin) };
        // SAFETY: the node follows the header inside the block.
        let node = unsafe { block.add(HEADER) };
        Ok(NonNull::slice_from_raw_parts(node, layout.size()))
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: the node came from `allocate`, which put the header in
        // front of it.
        let block = unsafe { ptr.sub(HEADER) };
        // SAFETY: as above.
        match unsafe { block.cast::<u64>().read() } {
            FROM_RESERVE => RESERVE.lock().push(block),
            _ => {
                let heap = heap_layout(layout).expect("the layout was allocated");
                // SAFETY: the block came from the heap with this layout.
                unsafe { alloc::alloc::dealloc(block.as_ptr(), heap) };
            }
        }
    }
}

// SAFETY: copies of `Nodes` allocate from the same heap and reserve.
unsafe impl AllocatorClone for Nodes {}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::Cell;

    std::thread_local! {
        /// Makes this thread's heap allocations for nodes fail.
        pub static HEAP_FULL: Cell<bool> = const { Cell::new(false) };
        /// Makes this thread's claims fail.
        pub static CLAIMS_FAIL: Cell<bool> = const { Cell::new(false) };
    }

    /// Runs `f` with every claim on this thread failing.
    pub(crate) fn without_claims<R>(f: impl FnOnce() -> R) -> R {
        CLAIMS_FAIL.with(|fail| fail.set(true));
        let result = f();
        CLAIMS_FAIL.with(|fail| fail.set(false));
        result
    }

    fn heap_full(full: bool) {
        HEAP_FULL.with(|f| f.set(full));
    }

    #[test]
    fn claimed_insertions_succeed_without_heap() {
        let mut map: Map<u64, u64> = map();
        for i in 0..1000 {
            let _claim = claim_insert().unwrap();
            heap_full(i % 2 == 1);
            map.insert(i, i * 2);
            heap_full(false);
        }
        assert_eq!(map.len(), 1000);
        assert!(map.iter().all(|(&k, &v)| v == k * 2));
        heap_full(true);
        for i in 0..1000 {
            assert_eq!(map.remove(&i), Some(i * 2));
        }
        heap_full(false);
    }

    #[test]
    fn claims_fail_without_heap_and_reserve() {
        let _held = claim(INSERT).unwrap();
        heap_full(true);
        assert!(claim(100_000).is_err());
        heap_full(false);
    }

    #[test]
    fn largest_nodes_fit_in_a_block() {
        use crate::regions::Region;
        // A node of a map with the largest entries the kernel keeps:
        // leaf contents plus the child pointers of an internal node.
        let entry = size_of::<u64>() + size_of::<Region<std::sync::Arc<()>>>();
        let node = 16 + 11 * entry + 12 * size_of::<usize>();
        assert!(node + HEADER <= BLOCK);
        let mut map: Map<u64, Region<std::sync::Arc<()>>> = map();
        for i in 0..200 {
            let _claim = claim_insert().unwrap();
            heap_full(true);
            map.insert(
                i,
                Region {
                    start: i,
                    len: 1,
                    offset: 0,
                    perms: 0,
                    max: 0,
                    inheritance: 0,
                    mem: std::sync::Arc::new(()),
                    removal: 0,
                },
            );
            heap_full(false);
        }
        assert_eq!(map.len(), 200);
    }
}
