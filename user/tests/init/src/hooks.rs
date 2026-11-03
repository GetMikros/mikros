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
pub const TESTS: [(&str, Test); 2] = [
    ("failed_allocations", failed_allocations),
    ("unqueued_exception_report", unqueued_exception_report),
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
