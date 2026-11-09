//! The kernel heap, in the kernel dynamic region.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use kernel_core::heap::{Heap, PAGE_SIZE, Pages};
use kernel_core::sync::SpinLock;

/// Pages mapped into the dynamic region on demand.
struct DynamicPages;

impl Pages for DynamicPages {
    fn alloc(&mut self, count: usize) -> Option<NonNull<u8>> {
        let start = crate::space::alloc_pages((count * PAGE_SIZE) as u64)?;
        NonNull::new(start as *mut u8)
    }

    fn free(&mut self, start: NonNull<u8>, count: usize) {
        crate::space::free_pages(start.as_ptr() as u64, (count * PAGE_SIZE) as u64);
    }
}

/// The global allocator. Its lock does not disable interrupts, so
/// interrupt handlers must not allocate.
struct KernelHeap(SpinLock<Heap<DynamicPages>>);

#[global_allocator]
static HEAP: KernelHeap = KernelHeap(SpinLock::new(
    kernel_core::lock_order::HEAP,
    Heap::new(DynamicPages),
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
        self.0
            .lock()
            .alloc(layout)
            .map_or(core::ptr::null_mut(), NonNull::as_ptr)
    }

    // SAFETY: the caller passes a block that `alloc` returned and the
    // layout it was allocated with, as `GlobalAlloc` requires.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(block) = NonNull::new(ptr) {
            // SAFETY: the caller passes a block from `alloc` with its layout.
            unsafe { self.0.lock().free(block, layout) };
        }
    }
}
