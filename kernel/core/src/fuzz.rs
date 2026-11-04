//! The test hooks of the fuzz build, which the `fuzz_*` system calls
//! other than the coverage calls set.

use crate::object::Object;
use crate::object::handles::Handle;
use crate::sched::Thread;
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

/// The kinds of allocation `fuzz_fail_alloc` makes fail: blocks of the
/// kernel heap and frames of the frame allocator.
pub const HEAP: u32 = 0;
pub const FRAMES: u32 = 1;

/// The allocations of a thread that are to fail: for each kind, the
/// number of allocations of that kind the thread makes in its own system
/// calls and faults until the one that fails, counting it, or 0 for none.
#[derive(Default)]
pub struct AllocFailures([AtomicU64; 2]);

/// Records the thread about to run on this processor, as its object ID
/// and allocation failures, or `None` when the processor is about to
/// idle.
pub fn switch_in(thread: Option<(u64, &AllocFailures)>) {
    let cpu = crate::cpu::this();
    let (koid, failures) = match thread {
        Some((koid, failures)) => (koid, failures as *const AllocFailures),
        None => (0, core::ptr::null()),
    };
    cpu.fuzz_running.store(koid, Ordering::Relaxed);
    cpu.fuzz_alloc_failures
        .store(failures.cast_mut().cast(), Ordering::Relaxed);
}

/// Checks that `kind` is a kind of allocation.
pub fn check_kind(kind: u32) -> Result<(), Error> {
    if kind != HEAP && kind != FRAMES {
        return Err(Error::InvalidArgs);
    }
    Ok(())
}

/// Takes back the failure set for `kind`, a known kind, on `thread`, and
/// returns how many allocations it still waited for, counting the one to
/// fail, or 0 when that one has failed or none was set.
pub fn take_alloc_failure(thread: &Thread, kind: u32) -> u64 {
    thread.alloc_failures().0[kind as usize].swap(0, Ordering::Relaxed)
}

/// Makes the `after`-th next allocation of `kind`, a known kind, that
/// `thread` makes fail, or none when `after` is 0.
pub fn fail_alloc(thread: &Thread, kind: u32, after: u64) {
    thread.alloc_failures().0[kind as usize].store(after, Ordering::Relaxed);
}

/// Whether the allocation of `kind` made now is to fail. Only the
/// allocations that the running thread makes outside interrupt and
/// exception handling, which also serves the maintenance deadlines, are
/// counted, and once one fails, no other of the kind does.
pub fn alloc_fails(kind: u32) -> bool {
    if crate::arch::handling_interrupt() {
        return false;
    }
    let failures = crate::cpu::this()
        .fuzz_alloc_failures
        .load(Ordering::Relaxed)
        .cast::<AllocFailures>();
    if failures.is_null() {
        return false;
    }
    // SAFETY: `switch_in` stored the failures of the thread running on
    // this processor, which lives while it runs, and outside interrupt
    // handling the code running is that thread's.
    let left = unsafe { &(*failures).0[kind as usize] };
    left.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1)) == Ok(1)
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

/// The points at which `fuzz_preempt_at` lets other threads run: between
/// the steps in which an address space checks the regions of a range,
/// and changes them, and in a channel write, after the handle values are
/// read and checked and before the handles are taken.
pub const REGION_CHECK: u32 = 1;
pub const REGION_CHANGE: u32 = 2;
pub const MESSAGE_HANDLES: u32 = 3;

/// `USER0`, which the event gets at the chosen pass, and `USER1`, which
/// the thread waits for there.
const USER0: u32 = 1 << 24;
const USER1: u32 = 1 << 25;

/// A pass of a point at which a thread is to stop.
struct Stop {
    /// The object ID of the thread.
    thread: u64,
    point: u32,
    /// The passes left, counting the one to stop at.
    count: u64,
    /// The event, the thread's handle to it, and how long to wait.
    event: Arc<dyn Object>,
    handle: Handle,
    deadline: i64,
}

static STOP: SpinLock<Option<Stop>> = SpinLock::new(None);

/// Makes `thread` stop at its `count`-th next pass of `point`, replacing
/// what was set before, or at none when `count` is 0. Fails with
/// `InvalidArgs` for an unknown point or without `event` for a nonzero
/// `count`.
pub fn preempt_at(
    thread: &Thread,
    point: u32,
    count: u64,
    event: Option<(Arc<dyn Object>, Handle)>,
    deadline: i64,
) -> Result<(), Error> {
    if !(REGION_CHECK..=MESSAGE_HANDLES).contains(&point) {
        return Err(Error::InvalidArgs);
    }
    let stop = match (count, event) {
        (0, _) => None,
        (_, Some((event, handle))) => Some(Stop {
            thread: thread.header().koid(),
            point,
            count,
            event,
            handle,
            deadline,
        }),
        (_, None) => return Err(Error::InvalidArgs),
    };
    let old = core::mem::replace(&mut *STOP.lock(), stop);
    // The event may go with it, which is done with no lock held.
    drop(old);
    Ok(())
}

/// A pass of `point` by the running thread. At the pass that
/// `preempt_at` chose, sets `USER0` on its event, which lets a thread
/// waiting for it run, and waits until `USER1` is set on it or the
/// deadline passes. No lock may be held.
pub fn pass(point: u32) {
    let running = crate::cpu::this().fuzz_running.load(Ordering::Relaxed);
    let stop = {
        let mut stop = STOP.lock();
        match &mut *stop {
            Some(s) if s.thread == running && s.point == point && s.count > 1 => {
                s.count -= 1;
                None
            }
            Some(s) if s.thread == running && s.point == point => stop.take(),
            _ => None,
        }
    };
    let Some(stop) = stop else {
        return;
    };
    let process = crate::sched::current().and_then(|t| t.process().cloned());
    stop.event.header().update_signals(0, USER0);
    if let Some(process) = process {
        let _ = crate::wait::wait_one(&stop.event, &process, stop.handle, USER1, stop.deadline);
    }
}
