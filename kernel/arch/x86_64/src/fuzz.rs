//! The test hooks of the fuzz build, which the `fuzz_*` system calls
//! other than the coverage calls set.

use crate::percpu;
use crate::sched::Thread;
use core::sync::atomic::{AtomicU64, Ordering};
use kernel_core::object::Object;
use kernel_core::status::Error;

/// The kinds of allocation `fuzz_fail_alloc` makes fail: blocks of the
/// kernel heap and frames of the frame allocator.
pub const HEAP: u32 = 0;
pub const FRAMES: u32 = 1;

/// For each kind of allocation, the object ID of the thread one of whose
/// allocations is to fail, or 0.
static FAIL_THREAD: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
/// For each kind, the number of allocations of that thread until the one
/// that fails, counting it.
static FAIL_AFTER: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// Records `thread`, the object ID of the thread about to run on this
/// processor, or 0 when it is about to idle.
pub fn switch_in(thread: u64) {
    percpu::this().running.store(thread, Ordering::Relaxed);
}

/// Takes back the failure set for `kind`, and returns how many
/// allocations it still waited for, counting the one to fail, or 0 when
/// that one has failed or none was set. Fails with `InvalidArgs` for an
/// unknown kind.
pub fn take_alloc_failure(kind: u32) -> Result<u64, Error> {
    if kind != HEAP && kind != FRAMES {
        return Err(Error::InvalidArgs);
    }
    let kind = kind as usize;
    match FAIL_THREAD[kind].swap(0, Ordering::Relaxed) {
        0 => Ok(0),
        _ => Ok(FAIL_AFTER[kind].load(Ordering::Relaxed)),
    }
}

/// Makes the `after`-th next allocation of `kind`, a known kind, that
/// `thread` makes fail, or none when `after` is 0.
pub fn fail_alloc(thread: &Thread, kind: u32, after: u64) {
    if after != 0 {
        FAIL_AFTER[kind as usize].store(after, Ordering::Relaxed);
        FAIL_THREAD[kind as usize].store(thread.header().koid(), Ordering::Relaxed);
    }
}

/// Whether the allocation of `kind` that the running thread makes now is
/// to fail. It is counted when `fail_alloc` chose the thread for the
/// kind, and once it fails, no other allocation of the kind does.
pub fn alloc_fails(kind: u32) -> bool {
    let kind = kind as usize;
    let thread = FAIL_THREAD[kind].load(Ordering::Relaxed);
    if thread == 0 || thread != percpu::this().running.load(Ordering::Relaxed) {
        return false;
    }
    if FAIL_AFTER[kind].fetch_sub(1, Ordering::Relaxed) > 1 {
        return false;
    }
    FAIL_THREAD[kind].store(0, Ordering::Relaxed);
    true
}
