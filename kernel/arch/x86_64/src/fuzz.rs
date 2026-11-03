//! The test hooks of the fuzz build, which the `fuzz_*` system calls
//! other than the coverage calls set.

use crate::percpu;
use crate::sched::Thread;
use core::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
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

/// The kernel's maintenance deadlines whose intervals `fuzz_set_interval`
/// sets: sampling the clock counter and reseeding the random number
/// generator.
pub const CLOCK: u32 = 0;
pub const RESEED: u32 = 1;

/// The shortest interval `fuzz_set_interval` sets, 1 ms.
const MIN_INTERVAL: i64 = 1_000_000;

/// For each deadline, the interval set, or 0 for the kernel's own.
static INTERVAL: [AtomicI64; 2] = [AtomicI64::new(0), AtomicI64::new(0)];
/// For each deadline, how often it was served, and when last.
static SERVED: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static LAST_SERVED: [AtomicI64; 2] = [AtomicI64::new(0), AtomicI64::new(0)];

/// The interval set for deadline `which`, if any.
pub fn interval(which: u32) -> Option<i64> {
    let ns = INTERVAL[which as usize].load(Ordering::Relaxed);
    (ns != 0).then_some(ns)
}

/// Records that deadline `which` was served at `now`.
pub fn served(which: u32, now: i64) {
    SERVED[which as usize].fetch_add(1, Ordering::Relaxed);
    LAST_SERVED[which as usize].store(now, Ordering::Relaxed);
}

/// Sets the interval of deadline `which` to `ns` nanoseconds, keeps it
/// when `ns` is 0 and goes back to the kernel's own when it is negative,
/// and rearms the deadlines. Returns how often the deadline was served,
/// when last, and when it last succeeded, which for a reseed is when the
/// last successful one ended. Fails with `InvalidArgs` for an unknown
/// deadline or an interval below `MIN_INTERVAL`.
pub fn set_interval(which: u32, ns: i64) -> Result<[i64; 3], Error> {
    if (which != CLOCK && which != RESEED) || (1..MIN_INTERVAL).contains(&ns) {
        return Err(Error::InvalidArgs);
    }
    if ns != 0 {
        INTERVAL[which as usize].store(ns.max(0), Ordering::Relaxed);
        crate::sched::start_maintenance();
    }
    let last = LAST_SERVED[which as usize].load(Ordering::Relaxed);
    let succeeded = match which {
        RESEED => crate::random::last_reseed(),
        _ => last,
    };
    let served = SERVED[which as usize].load(Ordering::Relaxed) as i64;
    Ok([served, last, succeeded])
}

/// The hardware random sources whose instructions `fuzz_fail_random`
/// makes report failure.
pub const RDSEED: u32 = 1 << 0;
pub const RDRAND: u32 = 1 << 1;

/// The sources that report failure.
static RANDOM_FAILS: AtomicU32 = AtomicU32::new(0);

/// Makes the instructions of `sources` report failure from now on, and
/// those of the others succeed again. Fails with `InvalidArgs` for an
/// unknown source.
pub fn fail_random(sources: u32) -> Result<(), Error> {
    if sources & !(RDSEED | RDRAND) != 0 {
        return Err(Error::InvalidArgs);
    }
    RANDOM_FAILS.store(sources, Ordering::Relaxed);
    Ok(())
}

/// Whether the instruction of `source` is to report failure.
pub fn random_fails(source: u32) -> bool {
    RANDOM_FAILS.load(Ordering::Relaxed) & source != 0
}
