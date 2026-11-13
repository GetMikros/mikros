//! The kernel's objects, as a system test: information records, events and waits, timers, ports, timelines, streams, random bytes and the clock.

#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::Ordering;
use mikros_rt::Managed;
use mikros_rt::sys::{self, Handle};
use mikros_test::*;

mikros_rt::managed_main!(main);

fn main(m: &Managed) -> i64 {
    mikros_test::run(
        m,
        &[
            ("info_topics", info_topics),
            ("events_and_waits", events_and_waits),
            ("timers", timers),
        ],
    )
}

/// The process and thread records describe the running test.
fn info_topics(s: &Own) -> Result {
    let process = info::<0x28>(s.process, 2)?;
    ensure("checking the process state", u32_at(&process, 0x08) == 1)?;
    ensure("checking the thread count", u32_at(&process, 0x0C) >= 1)?;
    ensure(
        "checking the process CPU time",
        i64::from_le_bytes(process[0x18..0x20].try_into().unwrap()) > 0,
    )?;
    let thread = info::<0x18>(s.thread, 3)?;
    ensure("checking the thread state", u32_at(&thread, 0x00) == 1)?;
    ensure("checking the priority", u32_at(&thread, 0x04) == 16)?;
    ensure("checking the exception type", u32_at(&thread, 0x08) == 0)?;
    ensure(
        "checking the thread CPU time",
        i64::from_le_bytes(thread[0x10..0x18].try_into().unwrap()) > 0,
    )
}

extern "sysv64" fn signal_later(event: u64, _: u64) -> ! {
    sleep_ms(20);
    // SAFETY: the call takes no pointers.
    unsafe { sys::object_signal(event as Handle, 0, USER1) };
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Events, waits on them and on threads, and cancelled waits.
fn events_and_waits(s: &Own) -> Result {
    let mut event = 0;
    // SAFETY: the pointer is valid for the call.
    check("creating an event", unsafe {
        sys::event_create(0, &mut event)
    })?;
    let (status, observed) = wait(event, SIGNALED, now() + 5_000_000);
    ensure("timing out", status == TIMED_OUT && observed == 0)?;
    // SAFETY: the call takes no pointers.
    check("signalling", unsafe {
        sys::object_signal(event, 0, SIGNALED | USER0)
    })?;
    let (status, observed) = wait(event, SIGNALED, i64::MAX);
    ensure(
        "seeing the signal",
        status == 0 && observed == SIGNALED | USER0,
    )?;
    // SAFETY: the call takes no pointers.
    let bad = unsafe { sys::object_signal(event, 0, 1) };
    ensure("refusing other bits", bad == -10)?;
    // SAFETY: the call takes no pointers.
    let wrong = unsafe { sys::object_signal(s.process, 0, USER0) };
    ensure("refusing other types", wrong == -12)?;
    // SAFETY: the call takes no pointers.
    check("clearing", unsafe {
        sys::object_signal(event, SIGNALED | USER0, 0)
    })?;

    let helper = spawn(s, signal_later, u64::from(event))?;
    let (status, observed) = wait(event, USER1, now() + 2_000_000_000);
    ensure(
        "being woken by another thread",
        status == 0 && observed == USER1,
    )?;
    let (status, _) = wait(helper, TERMINATED, now() + 2_000_000_000);
    ensure("seeing the thread end", status == 0)?;

    let mut copy = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(event, 0x8000_0000, &mut copy) };
    check("duplicating the event", status)?;
    let waiter = spawn(s, wait_forever, u64::from(copy))?;
    sleep_ms(20);
    // SAFETY: the call takes no pointers.
    check("closing the handle", unsafe { sys::handle_close(copy) })?;
    let (status, _) = wait(waiter, TERMINATED, now() + 2_000_000_000);
    ensure("seeing the waiter end", status == 0)?;
    ensure(
        "cancelling the wait",
        HELPER.load(Ordering::Acquire) as i64 == CANCELED,
    )
}

/// Timers set SIGNALED at their deadline, at once for a past one, and
/// not after being cancelled.
fn timers(_: &Own) -> Result {
    let mut timer = 0;
    // SAFETY: the pointer is valid for the call.
    check("creating a timer", unsafe {
        sys::timer_create(0, &mut timer)
    })?;
    let start = now();
    // SAFETY: the call takes no pointers.
    check("setting the timer", unsafe {
        sys::timer_set(timer, start + 10_000_000)
    })?;
    let (status, _) = wait(timer, SIGNALED, start + 2_000_000_000);
    ensure("firing", status == 0 && now() - start >= 10_000_000)?;
    // SAFETY: the call takes no pointers.
    check("setting a past deadline", unsafe {
        sys::timer_set(timer, 0)
    })?;
    let (status, _) = wait(timer, SIGNALED, 0);
    ensure("firing at once", status == 0)?;
    // SAFETY: the call takes no pointers.
    check("setting again", unsafe {
        sys::timer_set(timer, now() + 5_000_000)
    })?;
    // SAFETY: the call takes no pointers.
    check("cancelling", unsafe { sys::timer_cancel(timer) })?;
    let (status, observed) = wait(timer, SIGNALED, now() + 30_000_000);
    ensure(
        "staying clear after cancel",
        status == TIMED_OUT && observed == 0,
    )
}
