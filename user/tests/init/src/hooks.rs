//! The tests that use the test hooks of the fuzz build.

use crate::{
    READABLE, TERMINATE, TERMINATED, Test, act, budget_kernel, budget_pages, create_memory,
    exit_code, get_thread, in_time, koid, mem_read_at, now, report_bytes, start_program_with,
    thread_list, touch_stack, u32_at, wait,
};
use core::cell::Cell;
use mikros_rt::sys::{self, Handle};
use mikros_rt::{Startup, log};
use mikros_test::{Failure, PAGE_SIZE, PERM_READ, Result, check, ensure, sleep_ms};

/// The tests, which `main` runs before the last of its own.
pub const TESTS: [(&str, Test); 4] = [
    ("failed_allocations", failed_allocations),
    ("unqueued_exception_report", unqueued_exception_report),
    ("maintenance_deadlines", maintenance_deadlines),
    ("failed_random_sources", failed_random_sources),
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
