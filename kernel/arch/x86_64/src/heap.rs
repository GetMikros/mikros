//! The kernel heap, in the kernel dynamic region.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use kernel_core::heap::{Heap, PAGE_SIZE, Pages};
use kernel_core::sync::SpinLock;

/// The most whole-page blocks freed with the heap locked that wait to be
/// given back once it is unlocked.
const DEFERRED: usize = 8;

/// Pages mapped into the dynamic region on demand. Pages freed with the
/// heap locked are given back once it is unlocked, as unmapping them
/// waits for every processor to drop their translations, which must not
/// hold up every other allocation.
struct DynamicPages {
    /// The start and page count of each block waiting to be given back.
    deferred: [(usize, usize); DEFERRED],
    count: usize,
}

impl DynamicPages {
    /// Takes the blocks waiting to be given back.
    fn take_deferred(&mut self) -> ([(usize, usize); DEFERRED], usize) {
        let taken = (self.deferred, self.count);
        self.count = 0;
        taken
    }
}

/// Gives back the pages of the blocks `deferred` took. The heap must not
/// be locked.
fn give_back((blocks, count): ([(usize, usize); DEFERRED], usize)) {
    for &(start, pages) in &blocks[..count] {
        crate::space::free_pages(start as u64, (pages * PAGE_SIZE) as u64);
    }
}

impl Pages for DynamicPages {
    fn alloc(&mut self, count: usize) -> Option<NonNull<u8>> {
        let start = crate::space::alloc_pages((count * PAGE_SIZE) as u64)?;
        NonNull::new(start as *mut u8)
    }

    fn free(&mut self, start: NonNull<u8>, count: usize) {
        match self.deferred.get_mut(self.count) {
            Some(slot) => {
                *slot = (start.as_ptr() as usize, count);
                self.count += 1;
            }
            // Without room, the pages go at once.
            None => crate::space::free_pages(start.as_ptr() as u64, (count * PAGE_SIZE) as u64),
        }
    }

    #[cfg(feature = "diag")]
    fn records(&mut self) -> (usize, &mut [u32]) {
        // SAFETY: only the heap, behind its lock, reaches the records.
        let records = unsafe { &mut *RECORDS.0.get() };
        (crate::space::DYNAMIC_START as usize, records)
    }
}

/// The records of the heap check, for the first 4 GiB of the dynamic
/// region, where its pages come first.
#[cfg(feature = "diag")]
struct Records(core::cell::UnsafeCell<[u32; 1 << 20]>);

// SAFETY: `DynamicPages::records` is the only way to the records, and the
// heap's lock serializes it.
#[cfg(feature = "diag")]
unsafe impl Sync for Records {}

#[cfg(feature = "diag")]
static RECORDS: Records = Records(core::cell::UnsafeCell::new([0; 1 << 20]));

/// In the diagnostic build, the heap's use by class and its whole pages,
/// or `None` without `wait` while the heap is locked.
#[cfg(feature = "diag")]
pub fn usage(wait: bool) -> Option<([(usize, usize); kernel_core::heap::CLASSES], usize)> {
    let heap = match wait {
        true => Some(HEAP.0.lock()),
        false => HEAP.0.try_lock(),
    };
    heap.map(|h| h.usage())
}

/// The global allocator. Its lock does not disable interrupts, so
/// interrupt handlers must not allocate.
struct KernelHeap(SpinLock<Heap<DynamicPages>>);

#[global_allocator]
static HEAP: KernelHeap = KernelHeap(SpinLock::new(
    kernel_core::lock_order::HEAP,
    Heap::new(DynamicPages {
        deferred: [(0, 0); DEFERRED],
        count: 0,
    }),
));

// SAFETY: blocks come from `Heap`, which hands each out once until it is
// freed, with the size and alignment of the layout, and `dealloc` passes
// back the layout the block was allocated with.
unsafe impl GlobalAlloc for KernelHeap {
    // SAFETY: the caller passes a layout of nonzero size, as `GlobalAlloc`
    // requires, and the heap hands out each block once until it is freed.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        #[cfg(feature = "fuzz")]
        if crate::fuzz::alloc_fails(crate::fuzz::HEAP) {
            return core::ptr::null_mut();
        }
        #[cfg(not(feature = "diag"))]
        let block = self.0.lock().alloc(layout);
        // A failed check is reported once the lock is free, so that the
        // report cannot wait for it.
        #[cfg(feature = "diag")]
        let block = {
            let (block, fault) = {
                let mut heap = self.0.lock();
                (heap.alloc(layout), heap.fault.take())
            };
            if let Some(fault) = fault {
                panic!("{fault} allocating {layout:?}");
            }
            block
        };
        block.map_or(core::ptr::null_mut(), NonNull::as_ptr)
    }

    // SAFETY: the caller passes a block that `alloc` returned and the
    // layout it was allocated with, as `GlobalAlloc` requires.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(block) = NonNull::new(ptr) {
            #[cfg(not(feature = "diag"))]
            {
                let deferred = {
                    let mut heap = self.0.lock();
                    // SAFETY: the caller passes a block from `alloc` with
                    // its layout.
                    unsafe { heap.free(block, layout) };
                    heap.pages_mut().take_deferred()
                };
                give_back(deferred);
            }
            #[cfg(feature = "diag")]
            {
                let (fault, deferred) = {
                    let mut heap = self.0.lock();
                    // SAFETY: the caller passes a block from `alloc` with
                    // its layout.
                    unsafe { heap.free(block, layout) };
                    (heap.fault.take(), heap.pages_mut().take_deferred())
                };
                give_back(deferred);
                if let Some(fault) = fault {
                    panic!("{fault} freeing {layout:?}");
                }
            }
        }
    }
}
