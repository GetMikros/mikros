//! The global allocator, on memory objects mapped into the process.
//!
//! Until the program knows its process handle, allocations come from a
//! static arena, so that receiving the startup state can allocate.
//! Afterwards, small blocks come from regions of `REGION_SIZE` bytes,
//! each a memory object of its own. A block larger than `SMALL_MAX` gets
//! a mapping of its own that freeing unmaps, except that a block up to
//! `LARGE_MAX` bytes has a mapping of a power-of-two number of pages, and
//! a freed one stays mapped, up to `KEEP_MAX` bytes of them in all, for
//! the next block of its size: mapping and unmapping cost far more than
//! a call to a service.
//!
//! Small blocks are kept in free lists by size class, a power of two
//! from `MIN_BLOCK` to `SMALL_MAX`. A block of a class is aligned to its
//! size, so a class serves every alignment up to its size.

use crate::sys;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const PAGE_SIZE: usize = 4096;
const MIN_BLOCK: usize = 16;
const SMALL_MAX: usize = 2048;
const CLASSES: usize = (SMALL_MAX / MIN_BLOCK).trailing_zeros() as usize + 1;
const REGION_SIZE: usize = 64 * 1024;
/// The largest block whose mapping is kept when it is freed, the classes
/// of such blocks (one page, two, four, ...), and the most bytes of
/// freed mappings that are kept.
const LARGE_MAX: usize = 64 * 1024;
const LARGE_CLASSES: usize = (LARGE_MAX / PAGE_SIZE).trailing_zeros() as usize + 1;
const KEEP_MAX: usize = 256 * 1024;
const ARENA_SIZE: usize = 64 * 1024;

const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;

/// The process handle that memory is mapped with, 0 until it is known.
static PROCESS: AtomicU32 = AtomicU32::new(0);

/// Makes the allocator map memory into `process` once the static arena
/// is used up. The handle must stay open while the program runs.
pub fn set_process(process: sys::Handle) {
    PROCESS.store(process, Ordering::Release);
}

#[repr(C, align(4096))]
struct Arena(UnsafeCell<[u8; ARENA_SIZE]>);

/// What the lock guards.
struct State {
    free: [Option<NonNull<Free>>; CLASSES],
    /// The freed mappings that are kept, by class, and their bytes.
    large: [Option<NonNull<Free>>; LARGE_CLASSES],
    kept: usize,
    /// The unused part of the current region.
    next: usize,
    end: usize,
    arena_used: bool,
}

/// A free block, linked into the list of its class.
struct Free {
    next: Option<NonNull<Free>>,
}

pub struct Heap {
    locked: AtomicBool,
    state: UnsafeCell<State>,
    arena: Arena,
}

// SAFETY: `state` is only touched with `locked` held, and the arena only
// through `state`.
unsafe impl Sync for Heap {}

#[cfg(feature = "runtime")]
#[global_allocator]
static HEAP: Heap = Heap {
    locked: AtomicBool::new(false),
    state: UnsafeCell::new(State {
        free: [None; CLASSES],
        large: [None; LARGE_CLASSES],
        kept: 0,
        next: 0,
        end: 0,
        arena_used: false,
    }),
    arena: Arena(UnsafeCell::new([0; ARENA_SIZE])),
};

/// The size class of a small `layout`, or `None` for a large one.
fn class(layout: Layout) -> Option<usize> {
    let size = layout.size().max(layout.align()).max(MIN_BLOCK);
    if size > SMALL_MAX {
        return None;
    }
    let size = size.next_power_of_two();
    Some((size / MIN_BLOCK).trailing_zeros() as usize)
}

/// The class of a large `layout` whose mapping is kept when freed, or
/// `None` for a larger one or one aligned to more than a page.
fn large_class(layout: Layout) -> Option<usize> {
    let size = layout.size().max(PAGE_SIZE);
    if size > LARGE_MAX || layout.align() > PAGE_SIZE {
        return None;
    }
    Some((size.next_power_of_two() / PAGE_SIZE).trailing_zeros() as usize)
}

/// Maps `len` bytes, a whole number of pages, of a new memory object.
fn map(len: usize) -> Option<usize> {
    let process = PROCESS.load(Ordering::Acquire);
    if process == 0 {
        return None;
    }
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::mem_create(len as u64, 0, &mut mem) } != crate::OK {
        return None;
    }
    let mut addr = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let status = unsafe {
        sys::vm_map(
            process,
            PERM_READ | PERM_WRITE,
            0,
            mem,
            0,
            len as u64,
            &mut addr,
        )
    };
    // The mapping keeps the memory object alive.
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(mem) };
    (status == crate::OK).then_some(addr as usize)
}

/// The heap's state while its lock is held.
struct Guard<'a> {
    heap: &'a Heap,
}

impl core::ops::Deref for Guard<'_> {
    type Target = State;
    fn deref(&self) -> &State {
        // SAFETY: the guard holds the lock.
        unsafe { &*self.heap.state.get() }
    }
}

impl core::ops::DerefMut for Guard<'_> {
    fn deref_mut(&mut self) -> &mut State {
        // SAFETY: the guard holds the lock, and this is its only mutable
        // borrow.
        unsafe { &mut *self.heap.state.get() }
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.heap.locked.store(false, Ordering::Release);
    }
}

impl Heap {
    fn lock(&self) -> Guard<'_> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Guard { heap: self }
    }

    fn in_arena(&self, addr: usize) -> bool {
        let start = self.arena.0.get() as usize;
        (start..start + ARENA_SIZE).contains(&addr)
    }

    /// A block of a mapping of `PAGE_SIZE << c` bytes, a kept one when
    /// there is one, and whether it is new, and so zero.
    fn take_large(&self, c: usize) -> (*mut u8, bool) {
        let kept = {
            let mut s = self.lock();
            s.large[c].inspect(|free| {
                // SAFETY: a kept block holds a `Free`.
                s.large[c] = unsafe { free.as_ref().next };
                s.kept -= PAGE_SIZE << c;
            })
        };
        if let Some(block) = kept {
            return (block.as_ptr().cast(), false);
        }
        // Kept mappings still count against the process, so a mapping that
        // fails is tried again once they are given back.
        let mapped = map(PAGE_SIZE << c).or_else(|| {
            self.release_kept();
            map(PAGE_SIZE << c)
        });
        (mapped.map_or(ptr::null_mut(), |addr| addr as *mut u8), true)
    }

    /// Unmaps every kept mapping.
    fn release_kept(&self) {
        loop {
            let (block, len) = {
                let mut s = self.lock();
                let Some(c) = (0..LARGE_CLASSES).find(|&c| s.large[c].is_some()) else {
                    return;
                };
                let block = s.large[c].take().expect("a kept block");
                // SAFETY: a kept block holds a `Free`.
                s.large[c] = unsafe { block.as_ref().next };
                s.kept -= PAGE_SIZE << c;
                (block, PAGE_SIZE << c)
            };
            // SAFETY: the block is a kept mapping of `len` bytes that
            // nothing uses.
            unsafe {
                sys::vm_unmap(
                    PROCESS.load(Ordering::Acquire),
                    block.as_ptr() as u64,
                    len as u64,
                )
            };
        }
    }

    /// A new block of class `c` from the current region, which is
    /// replaced when it has no room.
    fn carve(&self, s: &mut State, c: usize) -> *mut u8 {
        let size = MIN_BLOCK << c;
        let mut at = s.next.next_multiple_of(size);
        if at + size > s.end || s.end == 0 {
            let start = if !s.arena_used && PROCESS.load(Ordering::Acquire) == 0 {
                s.arena_used = true;
                self.arena.0.get() as usize
            } else {
                match map(REGION_SIZE) {
                    Some(start) => start,
                    None if !s.arena_used => {
                        s.arena_used = true;
                        self.arena.0.get() as usize
                    }
                    None => return ptr::null_mut(),
                }
            };
            let len = if self.in_arena(start) {
                ARENA_SIZE
            } else {
                REGION_SIZE
            };
            s.next = start;
            s.end = start + len;
            at = start;
        }
        s.next = at + size;
        at as *mut u8
    }
}

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match class(layout) {
            Some(c) => {
                let mut s = self.lock();
                match s.free[c] {
                    Some(free) => {
                        // SAFETY: blocks on a free list are free and hold
                        // a `Free`.
                        s.free[c] = unsafe { free.as_ref().next };
                        free.as_ptr().cast()
                    }
                    None => self.carve(&mut s, c),
                }
            }
            None => match large_class(layout) {
                Some(c) => self.take_large(c).0,
                None if layout.align() <= PAGE_SIZE => {
                    let len = layout.size().next_multiple_of(PAGE_SIZE);
                    map(len).map_or(ptr::null_mut(), |addr| addr as *mut u8)
                }
                None => ptr::null_mut(),
            },
        }
    }

    /// A block of zeros. A new mapping is zero already, so only a small
    /// block or a kept mapping is cleared.
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let (block, fresh) = match large_class(layout).filter(|_| class(layout).is_none()) {
            Some(c) => self.take_large(c),
            // SAFETY: the caller's layout is passed on.
            None => (unsafe { self.alloc(layout) }, false),
        };
        if !block.is_null() && !fresh {
            // SAFETY: the block has `layout.size()` bytes.
            unsafe { block.write_bytes(0, layout.size()) };
        }
        block
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        match class(layout) {
            Some(c) => {
                let mut s = self.lock();
                let free = block.cast::<Free>();
                // SAFETY: the block is at least MIN_BLOCK bytes, aligned
                // to its size, and no longer in use.
                unsafe { free.write(Free { next: s.free[c] }) };
                s.free[c] = NonNull::new(free);
            }
            None => {
                if let Some(c) = large_class(layout) {
                    let len = PAGE_SIZE << c;
                    let mut s = self.lock();
                    if s.kept + len <= KEEP_MAX {
                        let free = block.cast::<Free>();
                        // SAFETY: the block is a mapping of `len` bytes
                        // that nothing uses any more.
                        unsafe { free.write(Free { next: s.large[c] }) };
                        s.large[c] = NonNull::new(free);
                        s.kept += len;
                        return;
                    }
                }
                let len = match large_class(layout) {
                    Some(c) => PAGE_SIZE << c,
                    None => layout.size().next_multiple_of(PAGE_SIZE),
                };
                // SAFETY: the block is a mapping of its own of `len`
                // bytes that nothing uses any more.
                unsafe { sys::vm_unmap(PROCESS.load(Ordering::Acquire), block as u64, len as u64) };
            }
        }
    }
}
