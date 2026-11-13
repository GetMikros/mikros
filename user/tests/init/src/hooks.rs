//! The tests that use the test hooks of the fuzz build.

use crate::{
    ACCESS_DENIED, INVALID, READABLE, TERMINATE, TERMINATED, Test, USER0, act, budget_kernel,
    budget_pages, create_memory, exit_code, get_thread, in_time, koid, map, mem_read_at, now,
    paged_object, report_bytes, signal, signal_restorer, skip_fault, spawn2, start_program_with,
    thread_list, touch_stack, u32_at, wait, wait_until_blocked, write_faults,
};
use core::cell::Cell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use mikros_rt::sys::{self, Handle};
use mikros_rt::{Startup, log};
use mikros_test::USER1;
use mikros_test::{
    Failure, PAGE_SIZE, PAGER, PERM_READ, PERM_WRITE, Result, SPECIFIC, check, ensure, seal,
    sleep_ms,
};

/// The tests, which `main` runs before the last of its own.
pub const TESTS: [(&str, Test); 7] = [
    ("failed_allocations", failed_allocations),
    (
        "allocation_failures_per_thread",
        allocation_failures_per_thread,
    ),
    ("unqueued_exception_report", unqueued_exception_report),
    ("maintenance_deadlines", maintenance_deadlines),
    ("failed_random_sources", failed_random_sources),
    (
        "protect_meets_seal_and_revocation",
        protect_meets_seal_and_revocation,
    ),
    ("message_handles_read_once", message_handles_read_once),
];

/// The kinds of allocation `fuzz_fail_alloc` makes fail.
const HEAP: u32 = 0;
const FRAMES: u32 = 1;
const NO_MEMORY: i64 = -4;

/// Makes the `after`-th next allocation of `kind` that `thread` makes
/// fail, or none when `after` is 0, and returns how many allocations the
/// failure set before still waited for, or 0 when it happened.
fn fail_alloc(thread: Handle, kind: u32, after: u64) -> Result<u64> {
    let mut left = 0;
    // SAFETY: the out pointer is valid for the call.
    check("choosing an allocation to fail", unsafe {
        sys::fuzz_fail_alloc(thread, kind, after, &mut left)
    })?;
    Ok(left)
}

/// The kernel memory and the pages charged to the test's budget.
fn charges(s: &Startup) -> Result<(u64, u64)> {
    Ok((budget_kernel(s)?, budget_pages(s)?))
}

/// Makes `call` with its first, second and further allocations of `kind`
/// failing in turn, until it makes fewer allocations than that, and
/// returns how many of its calls failed. A failed call must return
/// `ERR_NO_MEMORY` and leave the budget's charges as they were and
/// `unchanged` true. A call may also succeed when it took what failed
/// from memory set aside before, as insertions into the kernel's maps do;
/// `undo` then reverses it. The last call, which succeeds with every
/// allocation, is left as it is.
fn fail_each(
    s: &Startup,
    kind: u32,
    mut call: impl FnMut() -> i64,
    unchanged: impl Fn() -> Result<bool>,
    mut undo: impl FnMut() -> Result,
) -> Result<u64> {
    let before = charges(s)?;
    let mut failed = 0;
    for after in 1..=256 {
        fail_alloc(s.thread, kind, after)?;
        let status = call();
        if fail_alloc(s.thread, kind, 0)? != 0 {
            check("succeeding when no allocation fails", status)?;
            return Ok(failed);
        }
        match status {
            0 => undo()?,
            NO_MEMORY => {
                ensure("changing nothing", unchanged()?)?;
                failed += 1;
            }
            _ => {
                let what = "failing with ERR_NO_MEMORY";
                return Err(Failure { what, status });
            }
        }
        ensure("keeping the charges", charges(s)? == before)?;
    }
    ensure("reaching the end of the allocations", false)?;
    Ok(0)
}

fn close(handle: Handle) -> Result {
    // SAFETY: the call takes no pointers.
    check("closing", unsafe { sys::handle_close(handle) })
}

/// Calls that fail because a kernel allocation fails return
/// `ERR_NO_MEMORY` and change nothing, whichever of their allocations
/// fails, and the same calls succeed afterwards: creating events,
/// channels, processes and threads, writing a message with a handle,
/// committing a page of a memory object and mapping one.
fn failed_allocations(s: &Startup) -> Result {
    // Faults on the stack would allocate frames while one is to fail.
    touch_stack();
    let (out, other) = (Cell::new(0), Cell::new(0));
    let none = || Ok(out.get() == 0);
    let close_out = || close(out.get());
    let create_event = || {
        out.set(0);
        // SAFETY: the out pointer is valid for the call.
        unsafe { sys::event_create(0, out.as_ptr()) }
    };
    let n = fail_each(s, HEAP, create_event, none, close_out)?;
    ensure("allocating for an event", n > 0)?;
    close(out.get())?;

    let create_channel = || {
        out.set(0);
        other.set(0);
        // SAFETY: the out pointers are valid for the call.
        unsafe { sys::channel_create(0, out.as_ptr(), other.as_ptr()) }
    };
    let neither = || Ok(out.get() == 0 && other.get() == 0);
    let close_both = || close(out.get()).and_then(|()| close(other.get()));
    let n = fail_each(s, HEAP, create_channel, neither, close_both)?;
    ensure("allocating for a channel", n > 0)?;
    let (a, b) = (out.get(), other.get());

    let event = Cell::new(0);
    // SAFETY: the out pointer is valid for the call.
    check("creating an event", unsafe {
        sys::event_create(0, event.as_ptr())
    })?;
    let bytes = [7u8; 100];
    // SAFETY: the pointers describe `bytes` and the event's handle.
    let write = || unsafe { sys::channel_write(a, bytes.as_ptr(), 100, event.as_ptr(), 1) };
    let kept = || Ok(koid(event.get()).is_ok() && wait(b, READABLE, 0).1 & READABLE == 0);
    let read_back = || {
        let mut buf = [0u8; 100];
        let (mut len, mut count) = (0, 0);
        // SAFETY: the pointers describe the buffers.
        check("reading the message back", unsafe {
            sys::channel_read(
                b,
                buf.as_mut_ptr(),
                100,
                event.as_ptr(),
                1,
                &mut len,
                &mut count,
            )
        })
    };
    let n = fail_each(s, HEAP, write, kept, read_back)?;
    ensure("allocating for a message", n > 0)?;
    ensure("moving the handle once written", koid(event.get()).is_err())?;
    for handle in [a, b] {
        close(handle)?;
    }

    let mem = create_memory(2 * PAGE_SIZE)?;
    for (kind, page) in [(HEAP, 0), (FRAMES, 1)] {
        let offset = page * PAGE_SIZE;
        // SAFETY: the pointer and length describe the bytes.
        let commit = || unsafe { sys::mem_write(mem, offset, [1u8; 8].as_ptr(), 8) };
        let zero = || Ok(mem_read_at::<8>(mem, offset)? == [0; 8]);
        // SAFETY: the call takes no pointers.
        let decommit = || {
            check("decommitting", unsafe {
                sys::mem_op(mem, 2, offset, PAGE_SIZE)
            })
        };
        let n = fail_each(s, kind, commit, zero, decommit)?;
        ensure("allocating for a page", n > 0)?;
    }
    let at = Cell::new(0);
    let map_page = || {
        at.set(0);
        // SAFETY: the out pointer is valid for the call.
        unsafe { sys::vm_map(s.process, PERM_READ, 0, mem, 0, PAGE_SIZE, at.as_ptr()) }
    };
    // SAFETY: the call takes no pointers.
    let unmap = || {
        check("unmapping", unsafe {
            sys::vm_unmap(s.process, at.get(), PAGE_SIZE)
        })
    };
    let n = fail_each(s, HEAP, map_page, || Ok(at.get() == 0), unmap)?;
    ensure("allocating for a mapping", n > 0)?;
    unmap()?;
    close(mem)?;

    let name = b"failing";
    for kind in [HEAP, FRAMES] {
        let create_process = || {
            out.set(0);
            // SAFETY: the pointers describe `name` and `out`.
            unsafe { sys::process_create(s.budget, name.as_ptr(), 7, out.as_ptr()) }
        };
        let n = fail_each(s, kind, create_process, none, close_out)?;
        ensure("allocating for a process", n > 0)?;
        let process = out.get();
        let create_thread = || {
            other.set(0);
            // SAFETY: the pointers describe `name` and `other`.
            unsafe { sys::thread_create(process, name.as_ptr(), 7, other.as_ptr()) }
        };
        let close_other = || close(other.get());
        let n = fail_each(s, kind, create_thread, || Ok(other.get() == 0), close_other)?;
        ensure("allocating for a thread", n > 0)?;
        close(other.get())?;
        close(process)?;
    }
    Ok(())
}

/// Each thread keeps its own failure of each kind: setting one leaves the
/// others as they were, and reports only what was set on that thread for
/// that kind.
fn allocation_failures_per_thread(s: &Startup) -> Result {
    const FAR: u64 = 1 << 40;
    let name = b"other";
    let mut other = 0;
    // SAFETY: the pointers describe `name` and `other`.
    check("creating a thread", unsafe {
        sys::thread_create(s.process, name.as_ptr(), 5, &mut other)
    })?;
    // The test's own failure is far enough away that its calls below
    // cannot reach it.
    ensure(
        "starting without a failure",
        fail_alloc(s.thread, HEAP, FAR)? == 0,
    )?;
    ensure("keeping threads apart", fail_alloc(other, HEAP, 5)? == 0)?;
    ensure("keeping kinds apart", fail_alloc(other, FRAMES, 3)? == 0)?;
    ensure(
        "reporting the thread's heap failure",
        fail_alloc(other, HEAP, 0)? == 5,
    )?;
    ensure(
        "reporting the thread's frame failure",
        fail_alloc(other, FRAMES, 0)? == 3,
    )?;
    let left = fail_alloc(s.thread, HEAP, 0)?;
    ensure(
        "keeping the test's own failure",
        left > FAR - 1000 && left <= FAR,
    )?;
    close(other)
}

/// Waits until `process` has terminated or a message is on `channel`,
/// and returns whether it terminated.
fn terminated_or_readable(process: Handle, channel: Handle) -> Result<bool> {
    let deadline = in_time();
    loop {
        if wait(process, TERMINATED, 0).1 & TERMINATED != 0 {
            return Ok(true);
        }
        if wait(channel, READABLE, 0).1 & READABLE != 0 {
            return Ok(false);
        }
        ensure("terminating or reporting", now() < deadline)?;
        sleep_ms(1);
    }
}

/// When a kernel allocation for an exception report fails, the report
/// is not queued and the process ends with exit code -2. The debuggee
/// executes an undefined instruction with its first, second and further
/// heap allocations failing in turn, until that is reported. A failure
/// before it, while the debuggee's pages fault in, is reported as
/// `OUT_OF_MEMORY` unless the allocation is one of that report's.
fn unqueued_exception_report(s: &Startup) -> Result {
    const UNDEFINED_INSTRUCTION: u32 = 3;
    let mut unqueued = 0;
    for failed in 0..256 {
        let (mut channel, mut thread) = (0, 0);
        let (process, bootstrap) = start_program_with(s, "debuggee", 0, |process| {
            // SAFETY: the out pointer is valid for the call.
            check("creating an exception channel", unsafe {
                sys::exception_channel_create(process, 0, &mut channel)
            })?;
            let (koids, count) = thread_list(process)?;
            ensure("listing the thread", count == 1)?;
            let status;
            (status, thread) = get_thread(process, koids[0]);
            check("getting the thread", status)?;
            fail_alloc(thread, HEAP, failed + 1).map(drop)
        })?;
        let mut handles = [thread, channel, bootstrap, process, 0];
        let terminated = terminated_or_readable(process, channel)?;
        let reached = fail_alloc(thread, HEAP, 0)? == 0;
        if terminated {
            ensure("ending with exit code -2", exit_code(process)? == -2)?;
            ensure(
                "queuing no report",
                wait(channel, READABLE, 0).1 & READABLE == 0,
            )?;
            unqueued += 1;
        } else {
            let (report, reported) = report_bytes(channel)?;
            handles[4] = reported;
            check("terminating", act(reported, TERMINATE))?;
            ensure("ending with exit code -2", exit_code(process)? == -2)?;
            if !reached {
                log!(
                    Info,
                    "{unqueued} of {failed} failed allocations left a report unqueued"
                );
                ensure(
                    "reporting the undefined instruction",
                    u32_at(&report, 0) == UNDEFINED_INSTRUCTION,
                )?;
                ensure("failing to queue a report", unqueued >= 2)?;
                return handles.into_iter().try_for_each(close);
            }
        }
        handles
            .into_iter()
            .filter(|&h| h != 0)
            .try_for_each(close)?;
    }
    ensure("reporting once nothing fails", false)
}

/// The maintenance deadlines `fuzz_set_interval` sets.
const CLOCK: u32 = 0;
const RESEED: u32 = 1;
const MS: i64 = 1_000_000;

/// Sets the interval of maintenance deadline `which` to `ns`, keeps it
/// when `ns` is 0 or goes back to the kernel's own when `ns` is negative,
/// and returns how often the deadline was served, when last, and when it
/// last succeeded.
fn set_interval(which: u32, ns: i64) -> Result<[i64; 3]> {
    let mut report = [0; 3];
    // SAFETY: the pointer describes `report`.
    check("setting an interval", unsafe {
        sys::fuzz_set_interval(which, ns, report.as_mut_ptr())
    })?;
    Ok(report)
}

/// The kernel serves its clock sampling and reseed deadlines, each at its
/// own interval, while this thread sleeps and nothing else runs: with
/// intervals of 2 ms and 25 ms, both are served many times during a sleep
/// of 200 ms, the reseed no more often than its interval allows, and the
/// last successful reseed is at most two intervals old at the end.
fn maintenance_deadlines(_: &Startup) -> Result {
    let start = now();
    let clock = set_interval(CLOCK, 2 * MS)?;
    let reseed = set_interval(RESEED, 25 * MS)?;
    sleep_ms(200);
    let end = now();
    let clock_after = set_interval(CLOCK, -1)?;
    let reseed_after = set_interval(RESEED, -1)?;
    let (sampled, reseeded) = (clock_after[0] - clock[0], reseed_after[0] - reseed[0]);
    ensure(
        "sampling the clock while idle",
        sampled >= 20 && clock_after[1] > start,
    )?;
    ensure(
        "reseeding while idle",
        reseeded >= 2 && reseed_after[1] > start,
    )?;
    ensure(
        "reseeding no more often than the interval",
        reseeded <= 1 + (now() - start) / (25 * MS),
    )?;
    ensure(
        "reseeding within the interval",
        reseed_after[2] >= end - 50 * MS,
    )?;
    let kept = set_interval(RESEED, 0)?;
    ensure(
        "going back to the kernel's interval",
        kept[0] == reseed_after[0] && set_interval(RESEED, 1).is_err(),
    )
}

/// The hardware random sources `fuzz_fail_random` makes fail.
const RDSEED: u32 = 1 << 0;
const RDRAND: u32 = 1 << 1;

/// Makes the instructions of `sources` report failure, and the others
/// succeed.
fn fail_random(sources: u32) -> Result {
    // SAFETY: the call takes no pointers.
    check("making random sources fail", unsafe {
        sys::fuzz_fail_random(sources)
    })
}

/// The hardware random sources CPUID reports.
fn random_sources() -> u32 {
    use core::arch::x86_64::{__cpuid, __cpuid_count};
    let rdseed = __cpuid(0).eax >= 7 && __cpuid_count(7, 0).ebx & (1 << 18) != 0;
    let rdrand = __cpuid(1).ecx & (1 << 30) != 0;
    let mut sources = 0;
    if rdseed {
        sources |= RDSEED;
    }
    if rdrand {
        sources |= RDRAND;
    }
    sources
}

/// Whether a reseed succeeds within 50 ms, at an interval of 5 ms, and
/// how many were tried meanwhile.
fn reseeded() -> Result<(bool, i64)> {
    let before = set_interval(RESEED, 0)?;
    sleep_ms(50);
    let after = set_interval(RESEED, 0)?;
    Ok((after[2] != before[2], after[0] - before[0]))
}

/// A failed RDSEED or RDRAND gives no output: while both fail, the
/// kernel's reseeds are tried and none succeeds, yet `system_get_random`
/// goes on returning new bytes; while one fails, a reseed succeeds with
/// the other alone, when CPUID reports it.
fn failed_random_sources(_: &Startup) -> Result {
    let sources = random_sources();
    ensure("finding a hardware random source", sources != 0)?;
    set_interval(RESEED, 5 * MS)?;
    fail_random(RDSEED | RDRAND)?;
    let (succeeded, tried) = reseeded()?;
    ensure("trying to reseed", tried >= 2)?;
    ensure("reseeding without output", !succeeded)?;
    let (mut a, mut b) = ([0u8; 64], [0u8; 64]);
    // SAFETY: the pointers and lengths describe the arrays.
    unsafe {
        check("drawing", sys::system_get_random(a.as_mut_ptr(), 64))?;
        check("drawing again", sys::system_get_random(b.as_mut_ptr(), 64))?;
    }
    ensure("drawing new bytes", a != b && a != [0; 64] && b != [0; 64])?;
    for (failing, other) in [(RDSEED, RDRAND), (RDRAND, RDSEED), (0, sources)] {
        if sources & other == 0 {
            continue;
        }
        fail_random(failing)?;
        ensure("reseeding from the other source", reseeded()?.0)?;
    }
    fail_random(0)?;
    set_interval(RESEED, -1).map(drop)
}

/// The points at which `fuzz_preempt_at` stops a thread.
const REGION_CHECK: u32 = 1;
const REGION_CHANGE: u32 = 2;
const MESSAGE_HANDLES: u32 = 3;

/// Makes this thread stop at its `count`-th next pass of `point`, where
/// it sets `USER0` on `event` and waits until `USER1` is set on it, or at
/// none when `count` is 0.
fn preempt_at(point: u32, count: u64, event: Handle) -> Result {
    // SAFETY: the call takes no pointers.
    check("choosing a point to stop at", unsafe {
        sys::fuzz_preempt_at(point, count, event, in_time())
    })
}

/// What the helper of `protect_beside` does once it runs: seal its object
/// against writes, or revoke it through the pager in `PAGER`; and the
/// status of that call.
static REVOKING: AtomicBool = AtomicBool::new(false);
static ACTED: AtomicU64 = AtomicU64::new(0);

/// Waits for `USER0` on `event`, seals or revokes `mem`, and sets `USER1`.
extern "sysv64" fn act_at_stop(event: u64, mem: u64) -> ! {
    let (event, mem) = (event as Handle, mem as Handle);
    let mut status = wait(event, USER0, in_time()).0;
    if status == 0 {
        status = match REVOKING.load(Ordering::Acquire) {
            // SAFETY: the call takes no pointers.
            true => unsafe { sys::pager_revoke(PAGER.load(Ordering::Acquire) as Handle, mem) },
            false => seal(mem, 1 << 3),
        };
    }
    ACTED.store(status as u64, Ordering::Release);
    // SAFETY: the calls take no pointers.
    unsafe {
        sys::object_signal(event, 0, USER1);
        sys::thread_exit();
    }
    unreachable!("thread_exit returned");
}

/// The regions `protect_beside` makes, one page each.
const REGIONS: u64 = 513;

/// Maps `first` at the start of `REGIONS` read-only pages, `last` at
/// their end and one other object in between, and makes them writable
/// with `vm_protect` while a helper thread seals `first`, or revokes
/// `last`, when this thread passes `point` for the first time. Returns
/// the status of `vm_protect`, and the address of the pages.
fn protect_beside(s: &Startup, point: u32, first: Handle, last: Handle) -> Result<(i64, u64)> {
    let len = REGIONS * PAGE_SIZE;
    let range = create_memory(len)?;
    let base = map(s.process, range, len)?;
    // SAFETY: the call takes no pointers.
    check("freeing the range", unsafe {
        sys::vm_unmap(s.process, base, len)
    })?;
    let middle = create_memory(PAGE_SIZE)?;
    for page in 0..REGIONS {
        let mem = match page {
            0 => first,
            p if p == REGIONS - 1 => last,
            _ => middle,
        };
        let mut at = 0;
        let options = PERM_READ | SPECIFIC;
        let addr = base + page * PAGE_SIZE;
        // SAFETY: the out pointer is valid for the call.
        check("mapping a page", unsafe {
            sys::vm_map(s.process, options, addr, mem, 0, PAGE_SIZE, &mut at)
        })?;
    }
    let mut event = 0;
    // SAFETY: the out pointer is valid for the call.
    check("creating an event", unsafe {
        sys::event_create(0, &mut event)
    })?;
    let acted_on = if REVOKING.load(Ordering::Acquire) {
        last
    } else {
        first
    };
    let helper = spawn2(s, act_at_stop, u64::from(event), u64::from(acted_on))?;
    wait_until_blocked(helper)?;
    preempt_at(point, 1, event)?;
    let status = crate::protect(s, PERM_READ | PERM_WRITE, base, len);
    preempt_at(point, 0, INVALID)?;
    ensure(
        "ending the helper",
        wait(helper, TERMINATED, in_time()).0 == 0,
    )?;
    check("sealing or revoking", ACTED.load(Ordering::Acquire) as i64)?;
    ensure(
        "stopping at the point",
        wait(event, USER0 | USER1, 0).1 == USER0 | USER1,
    )?;
    for handle in [helper, event, middle, range] {
        close(handle)?;
    }
    Ok((status, base))
}

/// `vm_protect` fails with `ERR_ACCESS_DENIED` when a region's object is
/// sealed or revoked after the call checked the region, between two of
/// its steps, and still changes the other regions: a region sealed
/// between the steps that check the regions keeps its permissions, and
/// so does one revoked between the steps that change them.
fn protect_meets_seal_and_revocation(s: &Startup) -> Result {
    const SEGV: u32 = 11;
    let mut action = [0u8; 32];
    action[0x00..0x08].copy_from_slice(&(skip_fault as *const () as u64).to_le_bytes());
    action[0x10..0x18].copy_from_slice(&(signal_restorer as *const () as u64).to_le_bytes());
    let mut old = [0u8; 32];
    // SAFETY: the pointers describe `action` and `old`.
    check("catching SEGV", unsafe {
        sys::signal_set_action(SEGV, action.as_ptr(), old.as_mut_ptr())
    })?;
    let len = REGIONS * PAGE_SIZE;
    let result = (|| {
        let (first, last) = (create_memory(PAGE_SIZE)?, create_memory(PAGE_SIZE)?);
        REVOKING.store(false, Ordering::Release);
        let (status, base) = protect_beside(s, REGION_CHECK, first, last)?;
        ensure("refusing the sealed region", status == ACCESS_DENIED)?;
        ensure("keeping the sealed region", write_faults(base))?;
        ensure("changing the next region", !write_faults(base + PAGE_SIZE))?;
        ensure(
            "changing the last region",
            !write_faults(base + len - PAGE_SIZE),
        )?;
        // SAFETY: the call takes no pointers.
        check("unmapping", unsafe { sys::vm_unmap(s.process, base, len) })?;

        // The first object is sealed now; a new one takes its place.
        let unsealed = create_memory(PAGE_SIZE)?;
        let (pager, paged) = paged_object(s, 1, 0)?;
        REVOKING.store(true, Ordering::Release);
        let (status, base) = protect_beside(s, REGION_CHANGE, unsealed, paged)?;
        ensure("refusing the revoked region", status == ACCESS_DENIED)?;
        ensure("changing the first region", !write_faults(base))?;
        ensure("changing the next region", !write_faults(base + PAGE_SIZE))?;
        // SAFETY: the call takes no pointers.
        check("unmapping", unsafe { sys::vm_unmap(s.process, base, len) })?;
        for handle in [first, last, unsealed, paged, pager] {
            close(handle)?;
        }
        Ok(())
    })();
    // SAFETY: the pointer describes `old`.
    unsafe { sys::signal_set_action(SEGV, old.as_ptr(), core::ptr::null_mut()) };
    result
}

/// The handle value that `message_handles_read_once` writes from, and
/// that its helper changes.
static HANDLE_VALUE: AtomicU32 = AtomicU32::new(0);

/// Waits for `USER0` on `event`, puts `value` in `HANDLE_VALUE`, and sets
/// `USER1`.
extern "sysv64" fn change_handle_value(event: u64, value: u64) -> ! {
    if wait(event as Handle, USER0, in_time()).0 == 0 {
        HANDLE_VALUE.store(value as Handle, Ordering::Release);
    }
    // SAFETY: the calls take no pointers.
    unsafe {
        sys::object_signal(event as Handle, 0, USER1);
        sys::thread_exit();
    }
    unreachable!("thread_exit returned");
}

/// A channel write reads the handle values once: another thread changes
/// the value in user memory after the kernel read and checked it and
/// before it takes the handle, and the handle the caller named first is
/// the one that is moved.
fn message_handles_read_once(s: &Startup) -> Result {
    let (mut a, mut b, mut named, mut other, mut event) = (0, 0, 0, 0, 0);
    // SAFETY: the out pointers are valid for the calls.
    unsafe {
        check("creating a channel", sys::channel_create(0, &mut a, &mut b))?;
        check("creating an event", sys::event_create(0, &mut named))?;
        check("creating an event", sys::event_create(0, &mut other))?;
        check("creating an event", sys::event_create(0, &mut event))?;
    }
    let named_koid = koid(named)?;
    HANDLE_VALUE.store(named, Ordering::Release);
    let helper = spawn2(s, change_handle_value, u64::from(event), u64::from(other))?;
    wait_until_blocked(helper)?;
    preempt_at(MESSAGE_HANDLES, 1, event)?;
    let values = HANDLE_VALUE.as_ptr();
    // SAFETY: the handle array is the one value in `HANDLE_VALUE`.
    let status = unsafe { sys::channel_write(a, core::ptr::null(), 0, values, 1) };
    preempt_at(MESSAGE_HANDLES, 0, INVALID)?;
    check("writing the message", status)?;
    ensure(
        "ending the helper",
        wait(helper, TERMINATED, in_time()).0 == 0,
    )?;
    ensure(
        "changing the value meanwhile",
        HANDLE_VALUE.load(Ordering::Acquire) == other,
    )?;
    ensure("moving the named handle", koid(named).is_err())?;
    ensure("keeping the other handle", koid(other).is_ok())?;
    let mut received = 0;
    let (mut len, mut count) = (0, 0);
    // SAFETY: the pointers describe one handle and the sizes.
    check("reading the message", unsafe {
        sys::channel_read(
            b,
            core::ptr::null_mut(),
            0,
            &mut received,
            1,
            &mut len,
            &mut count,
        )
    })?;
    ensure(
        "sending the named object",
        count == 1 && koid(received)? == named_koid,
    )?;
    signal(event, USER0 | USER1, 0)?;
    for handle in [helper, a, b, other, event, received] {
        close(handle)?;
    }
    Ok(())
}
