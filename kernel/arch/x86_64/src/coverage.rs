//! Coverage collection for the fuzz build, in the format of Linux's kcov
//! `KCOV_TRACE_PC` mode: a `u64` count followed by the addresses.

use crate::memory_object::MemoryObject;
use crate::percpu::{self, COV_AREA_OFFSET, COV_CAPACITY_OFFSET, INTERRUPTS_OFFSET, READY};
use crate::sched::Thread;
use crate::space;
use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use kernel_core::object::memory::PAGE_SIZE;
use kernel_core::status::Error;

// The fuzz build calls this at the start of every instrumented basic
// block. It is written in assembly so that it is not instrumented
// itself. Until the GS base points to the per-CPU data, and during
// hardware interrupt handling, it does nothing. Otherwise it appends its
// return address to the running thread's buffer while the count is
// below the capacity. It changes only RAX, RCX, RDX and the flags, which
// a call may change.
core::arch::global_asm!(
    ".pushsection .text.sanitizer_cov_trace_pc, \"ax\"",
    ".global __sanitizer_cov_trace_pc",
    "__sanitizer_cov_trace_pc:",
    "cmp byte ptr [rip + {ready}], 0",
    "je 1f",
    "cmp qword ptr gs:[{interrupts}], 0",
    "jne 1f",
    "mov rax, gs:[{area}]",
    "test rax, rax",
    "jz 1f",
    "mov rcx, [rax]",
    "cmp rcx, gs:[{capacity}]",
    "jae 1f",
    "mov rdx, [rsp]",
    "mov [rax + rcx * 8 + 8], rdx",
    "inc rcx",
    "mov [rax], rcx",
    "1:",
    "ret",
    ".popsection",
    ready = sym READY,
    interrupts = const INTERRUPTS_OFFSET,
    area = const COV_AREA_OFFSET,
    capacity = const COV_CAPACITY_OFFSET,
);

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
        let Some(area) = space::map_frames(&frames) else {
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
            space::unmap_frames(self.area, self.len);
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
    let cpu = percpu::this();
    cpu.cov_area.store(0, Ordering::Relaxed);
    cpu.cov_capacity.store(capacity, Ordering::Relaxed);
    cpu.cov_area.store(area, Ordering::Relaxed);
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
    let cpu = percpu::this();
    cpu.cov_area.store(0, Ordering::Relaxed);
    let buffer = thread.coverage().lock().take();
    drop(buffer);
}
