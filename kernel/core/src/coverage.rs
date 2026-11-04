//! Coverage collection for the fuzz build, in the format of Linux's kcov
//! `KCOV_TRACE_PC` mode: a `u64` count followed by the addresses. The hook
//! that writes them is architecture code, reached through
//! `crate::arch::coverage_point`.

use crate::memory_object::MemoryObject;
use crate::object::memory::PAGE_SIZE;
use crate::sched::Thread;
use crate::status::Error;
use alloc::sync::Arc;

/// A thread's coverage buffer: an anonymous memory object whose frames
/// are pinned and mapped into kernel space for as long as this lives.
pub struct Buffer {
    mem: Arc<MemoryObject>,
    area: u64,
    /// The length of the mapping, whole pages.
    len: u64,
    /// The number of addresses that fit after the count in the object's
    /// size when coverage was enabled.
    capacity: u64,
}

impl Buffer {
    /// Pins and maps the whole of `mem`.
    fn new(mem: Arc<MemoryObject>) -> Result<Buffer, Error> {
        let (frames, size) = mem.pin_all()?;
        let len = frames.len() as u64 * PAGE_SIZE;
        let Some(area) = crate::arch::kernel_frames_map(&frames) else {
            mem.unpin_all();
            return Err(Error::NoMemory);
        };
        Ok(Buffer {
            mem,
            area,
            len,
            capacity: (size / 8).saturating_sub(1),
        })
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        if self.len != 0 {
            // SAFETY: the mapping came from `kernel_frames_map` in `new`,
            // and the hook no longer points at it once the buffer is gone.
            unsafe { crate::arch::kernel_frames_unmap(self.area, self.len) };
        }
        self.mem.unpin_all();
    }
}

/// Points the hook at the buffer of `thread`, which is about to run, or
/// at none.
pub fn switch_in(thread: &Thread) {
    let (area, capacity) = match &*thread.coverage().lock() {
        Some(b) if b.capacity != 0 => (b.area, b.capacity),
        _ => (0, 0),
    };
    crate::arch::coverage_point(area, capacity);
}

/// Points the hook at no buffer, as the processor is about to idle.
pub fn idle() {
    crate::arch::coverage_point(0, 0);
}

/// Starts collecting coverage for `thread`, the running thread, into
/// `mem`. Fails with `BadState` when it already collects.
pub fn enable(thread: &Thread, mem: Arc<MemoryObject>) -> Result<(), Error> {
    if thread.coverage().lock().is_some() {
        return Err(Error::BadState);
    }
    let buffer = Buffer::new(mem)?;
    *thread.coverage().lock() = Some(buffer);
    switch_in(thread);
    Ok(())
}

/// Stops collecting coverage for `thread`, the running thread.
pub fn disable(thread: &Thread) {
    crate::arch::coverage_point(0, 0);
    let buffer = thread.coverage().lock().take();
    drop(buffer);
}
