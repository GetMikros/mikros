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
            ("ports", ports),
            ("ports_are_readable", ports_are_readable),
            ("port_registration_limit", port_registration_limit),
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

fn wait_async(object: Handle, port: Handle, key: u64, signals: u32, options: u32) -> Result {
    // SAFETY: the call takes no pointers.
    let status = unsafe { sys::object_wait_async(object, port, key, signals, options) };
    check("registering a wait", status)
}

/// A port takes 65536 registrations and refuses the next with
/// `ERR_NO_RESOURCES`; closing the handle they were made through removes
/// them.
fn port_registration_limit(_: &Own) -> Result {
    const NO_RESOURCES: i64 = -3;
    const LIMIT: u64 = 65536;
    let (mut port, mut event) = (0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a port", sys::port_create(0, &mut port))?;
        check("creating an event", sys::event_create(0, &mut event))?;
    }
    for key in 0..LIMIT {
        wait_async(event, port, key, USER0, 0)?;
    }
    // SAFETY: the call takes no pointers.
    let status = unsafe { sys::object_wait_async(event, port, LIMIT, USER0, 0) };
    ensure("refusing one more", status == NO_RESOURCES)?;
    // SAFETY: the call takes no pointers.
    check("closing the event", unsafe { sys::handle_close(event) })?;
    // SAFETY: the pointer is valid for the call.
    check("creating another event", unsafe {
        sys::event_create(0, &mut event)
    })?;
    wait_async(event, port, 0, USER0, 0)?;
    // SAFETY: the calls take no pointers.
    unsafe {
        check("closing the event", sys::handle_close(event))?;
        check("closing the port", sys::handle_close(port))
    }
}

/// Ports with user packets, one-shot and edge waits, cancelling and
/// closing.
fn ports(_: &Own) -> Result {
    let (mut port, mut event) = (0, 0);
    // SAFETY: the pointers are valid for the calls.
    check("creating a port", unsafe { sys::port_create(0, &mut port) })?;
    // SAFETY: the pointer is valid for the call.
    check("creating an event", unsafe {
        sys::event_create(0, &mut event)
    })?;
    ensure(
        "timing out on an empty port",
        take_packet(port, soon()).0 == TIMED_OUT,
    )?;

    let mut user = [0u8; 48];
    user[..8].copy_from_slice(&7u64.to_le_bytes());
    user[8..16].copy_from_slice(&[9; 8]);
    user[16..20].copy_from_slice(b"ping");
    // SAFETY: the pointer describes `user`.
    check("queueing", unsafe { sys::port_queue(port, user.as_ptr()) })?;
    let (status, p) = take_packet(port, soon());
    ensure(
        "reading a user packet",
        status == 0 && u64_at(&p, 0) == 7 && u32_at(&p, 8) == 2,
    )?;
    ensure(
        "clearing the status",
        u32_at(&p, 12) == 0 && &p[16..20] == b"ping",
    )?;

    wait_async(event, port, 1, USER0, 0)?;
    ensure("not firing early", take_packet(port, soon()).0 == TIMED_OUT)?;
    signal(event, 0, USER0)?;
    let (status, p) = take_packet(port, soon());
    ensure(
        "firing",
        status == 0 && u64_at(&p, 0) == 1 && u32_at(&p, 8) == 1,
    )?;
    ensure(
        "reporting the signals",
        u32_at(&p, 16) == USER0 && u32_at(&p, 20) == USER0,
    )?;
    ensure("zeroing the rest", p[24..].iter().all(|&b| b == 0))?;
    signal(event, USER0, 0)?;
    signal(event, 0, USER0)?;
    ensure("firing only once", take_packet(port, soon()).0 == TIMED_OUT)?;
    wait_async(event, port, 2, USER0, 0)?;
    ensure("firing at once", take_packet(port, soon()).0 == 0)?;
    signal(event, USER0, 0)?;

    wait_async(event, port, 3, USER1, 1)?;
    signal(event, 0, USER1)?;
    signal(event, USER1, 0)?;
    signal(event, 0, USER1)?;
    let (status, p) = take_packet(port, soon());
    ensure("firing on an edge", status == 0 && u64_at(&p, 0) == 3)?;
    ensure(
        "queueing one packet at a time",
        take_packet(port, soon()).0 == TIMED_OUT,
    )?;
    signal(event, USER1, 0)?;
    signal(event, 0, USER1)?;
    ensure("firing on the next edge", take_packet(port, soon()).0 == 0)?;
    signal(event, USER1, 0)?;
    signal(event, 0, USER1)?;
    // SAFETY: the call takes no pointers.
    check("cancelling", unsafe { sys::port_cancel(port, event, 3) })?;
    ensure(
        "dropping the cancelled packet",
        take_packet(port, soon()).0 == TIMED_OUT,
    )?;
    signal(event, USER1, 0)?;

    // A one-shot registration that fired has ended, but its unread packet
    // is still cancelled.
    wait_async(event, port, 5, USER0, 0)?;
    signal(event, 0, USER0)?;
    // A new registration removes the ended one from the kernel's list.
    wait_async(event, port, 6, USER1, 0)?;
    // SAFETY: the calls take no pointers.
    unsafe {
        check("cancelling a fired wait", sys::port_cancel(port, event, 5))?;
        check("cancelling the other", sys::port_cancel(port, event, 6))?;
    }
    ensure(
        "dropping its packet",
        take_packet(port, soon()).0 == TIMED_OUT,
    )?;
    signal(event, USER0, 0)?;

    let mut copy = 0;
    // SAFETY: the pointer is valid for the call.
    check("duplicating", unsafe {
        sys::handle_duplicate(event, 0x8000_0000, &mut copy)
    })?;
    wait_async(copy, port, 4, USER1, 0)?;
    // SAFETY: the call takes no pointers.
    check("closing", unsafe { sys::handle_close(copy) })?;
    signal(event, 0, USER1)?;
    ensure(
        "ending with the handle",
        take_packet(port, soon()).0 == TIMED_OUT,
    )?;

    for _ in 0..4096 {
        // SAFETY: the pointer describes `user`.
        check("filling the port", unsafe {
            sys::port_queue(port, user.as_ptr())
        })?;
    }
    // SAFETY: the pointer describes `user`.
    let full = unsafe { sys::port_queue(port, user.as_ptr()) };
    ensure("limiting user packets", full == -22)
}

/// A port is `READABLE` while a packet is queued on it, which
/// `object_wait_one` and the waits of other ports see: a user packet and a
/// wait that fires on an event's signal make it readable, and reading the
/// last packet makes it unreadable. Two ports wait on each other here.
fn ports_are_readable(_: &Own) -> Result {
    const EDGE: u32 = 1;
    let (mut port, mut watcher, mut event) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a port", sys::port_create(0, &mut port))?;
        check("creating another", sys::port_create(0, &mut watcher))?;
        check("creating an event", sys::event_create(0, &mut event))?;
    }
    let readable = |p: Handle| {
        let mut observed = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::object_wait_one(p, READABLE, 0, &mut observed) };
        (status, observed)
    };
    let key = |p: Handle| {
        let (status, packet) = take_packet(p, soon());
        (status == 0).then(|| u64_at(&packet, 0))
    };
    ensure("waiting on an empty port", readable(port) == (TIMED_OUT, 0))?;
    wait_async(port, watcher, 1, READABLE, EDGE)?;
    wait_async(watcher, port, 2, READABLE, EDGE)?;
    ensure(
        "queueing nothing for empty ports",
        take_packet(watcher, 0).0 == TIMED_OUT,
    )?;
    let mut user = [0u8; 48];
    user[..8].copy_from_slice(&9u64.to_le_bytes());
    // SAFETY: the pointer describes `user`.
    check("queueing", unsafe { sys::port_queue(port, user.as_ptr()) })?;
    ensure("making it readable", readable(port) == (0, READABLE))?;
    // The watcher's packet made it readable, so the port got one too.
    ensure("telling the other port", key(watcher) == Some(1))?;
    ensure(
        "being told back",
        key(port) == Some(9) && key(port) == Some(2),
    )?;
    ensure("emptying it", readable(port) == (TIMED_OUT, 0))?;
    // A wait that fires on the event queues on the port, which the other
    // port hears of as before.
    wait_async(event, port, 3, USER0, 0)?;
    signal(event, 0, USER0)?;
    ensure("firing into it", readable(port) == (0, READABLE))?;
    ensure("telling the other port again", key(watcher) == Some(1))?;
    ensure(
        "being told back again",
        key(port) == Some(3) && key(port) == Some(2),
    )?;
    ensure("emptying it again", readable(port) == (TIMED_OUT, 0))?;
    // SAFETY: the calls take no pointers.
    unsafe {
        check("closing the event", sys::handle_close(event))?;
        check("closing a port", sys::handle_close(watcher))?;
        check("closing the other", sys::handle_close(port))
    }
}
