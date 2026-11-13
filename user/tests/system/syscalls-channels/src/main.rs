//! The kernel's channels and calls, as a system test: what a channel
//! refuses to send or read, how it reads into a full handle table, what
//! a failed call leaves in its outputs, the options a call refuses, and
//! the flags a call clears.

#![no_std]
#![no_main]

extern crate alloc;

use mikros_rt::Managed;
use mikros_rt::sys::{self, Handle};
use mikros_test::*;

mikros_rt::managed_main!(main);

fn main(m: &Managed) -> i64 {
    mikros_test::run(
        m,
        &[
            ("own_endpoints_not_sent", own_endpoints_not_sent),
            ("channel_errors", channel_errors),
            (
                "channel_read_into_a_full_table",
                channel_read_into_a_full_table,
            ),
            ("failed_outputs", failed_outputs),
            ("undefined_options_refused", undefined_options_refused),
            ("syscall_clears_flags", syscall_clears_flags),
        ],
    )
}

/// A channel's own endpoints cannot be sent through it, by a write or a
/// call, under any handle value.
fn own_endpoints_not_sent(_: &Own) -> Result {
    let (mut a, mut b) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    check("creating a channel", unsafe {
        sys::channel_create(0, &mut a, &mut b)
    })?;
    let mut copy = 0;
    // SAFETY: the out pointer is valid for the call.
    check("duplicating", unsafe {
        sys::handle_duplicate(a, 0x8000_0000, &mut copy)
    })?;
    let byte = [1u8];
    for (what, h) in [("the writing end", copy), ("its peer", b)] {
        // SAFETY: the pointers describe `byte` and `h`.
        let status = unsafe { sys::channel_write(a, byte.as_ptr(), 1, &h, 1) };
        ensure(what, status == INVALID_ARGS)?;
        ensure(what, call(a, &byte, Some(h), 64, soon()).0 == INVALID_ARGS)?;
    }
    ensure("keeping the handles", read_message(b, 1).is_err())?;
    for h in [a, b, copy] {
        // SAFETY: the call takes no pointers.
        check("closing", unsafe { sys::handle_close(h) })?;
    }
    Ok(())
}

/// A read whose handles the caller's table cannot hold fails with
/// `ERR_NO_RESOURCES`, stores the sizes and leaves the message queued; it
/// succeeds once the table has room.
fn channel_read_into_a_full_table(s: &Own) -> Result {
    const NO_RESOURCES: i64 = -3;
    const SAME_RIGHTS: u32 = 1 << 31;
    const LIMIT: u64 = 65536;
    let (mut a, mut b, mut event) = (0, 0, 0);
    // SAFETY: the out pointers are valid for the calls.
    unsafe {
        check("creating a channel", sys::channel_create(0, &mut a, &mut b))?;
        check("creating an event", sys::event_create(0, &mut event))?;
    }
    let mut sent = [0; 2];
    for copy in &mut sent {
        // SAFETY: the out pointer is valid for the call.
        check("duplicating", unsafe {
            sys::handle_duplicate(event, SAME_RIGHTS, copy)
        })?;
    }
    // SAFETY: the pointers describe the byte and `sent`.
    check("writing two handles", unsafe {
        sys::channel_write(a, b"x".as_ptr(), 1, sent.as_ptr(), 2)
    })?;
    // The duplicates that fill the table are kept in a mapped object.
    let size = LIMIT * 4;
    let store = create_memory(size)?;
    let at = map(s.process, store, size)?;
    let kept = at as *mut Handle;
    let mut count = 0;
    loop {
        let mut copy = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::handle_duplicate(event, SAME_RIGHTS, &mut copy) };
        if status == NO_RESOURCES {
            break;
        }
        check("filling the table", status)?;
        ensure("staying within the store", count < LIMIT)?;
        // SAFETY: the store holds `LIMIT` handles.
        unsafe { kept.add(count as usize).write(copy) };
        count += 1;
    }
    let close_last = |count: &mut u64| {
        *count -= 1;
        // SAFETY: the store holds the handle; the call takes no pointers.
        unsafe { sys::handle_close(kept.add(*count as usize).read()) };
    };
    close_last(&mut count);
    let refused = read_sizes(b, 16, 2);
    close_last(&mut count);
    let read = read_sizes(b, 16, 2);
    while count > 0 {
        close_last(&mut count);
    }
    ensure(
        "refusing a read without room for its handles",
        refused == (NO_RESOURCES, 1, 2),
    )?;
    ensure("reading it once there is room", read == (0, 1, 2))?;
    // SAFETY: the calls take no pointers.
    unsafe {
        sys::vm_unmap(s.process, at, size);
        for handle in [store, event, a, b] {
            sys::handle_close(handle);
        }
    }
    Ok(())
}

/// A refused `channel_write` queues nothing and keeps the caller's
/// handles; a written handle leaves the caller's table and arrives with
/// the rights it had; a read into buffers that are too small stores the
/// sizes and leaves the message queued; and a closed peer is reported.
fn channel_errors(s: &Own) -> Result {
    let (mut a, mut b, mut event) = (0, 0, 0);
    // SAFETY: the out pointers are valid for the calls.
    unsafe {
        check("creating a channel", sys::channel_create(0, &mut a, &mut b))?;
        check("creating an event", sys::event_create(0, &mut event))?;
    }
    // 65 handles, one more than a message holds, each transferable.
    let mut many = [0 as Handle; 65];
    for h in &mut many {
        // SAFETY: the out pointer is valid for the call.
        check("duplicating", unsafe {
            sys::handle_duplicate(event, 0x8000_0000, h)
        })?;
    }
    let big = 17 * PAGE_SIZE;
    let at = map(s.process, create_memory(big)?, big)?;
    // SAFETY: the bytes and handles lie in mapped memory.
    let refused = unsafe {
        [
            (
                "refusing 65537 bytes",
                sys::channel_write(a, at as *const u8, 65537, core::ptr::null(), 0),
                OUT_OF_RANGE,
            ),
            (
                "refusing 65 handles",
                sys::channel_write(a, at as *const u8, 1, many.as_ptr(), 65),
                OUT_OF_RANGE,
            ),
            (
                "refusing a handle twice",
                sys::channel_write(a, at as *const u8, 1, [event, event].as_ptr(), 2),
                INVALID_ARGS,
            ),
            (
                "refusing the channel's own handle",
                sys::channel_write(a, at as *const u8, 1, &a, 1),
                INVALID_ARGS,
            ),
        ]
    };
    for (what, status, expected) in refused {
        ensure(what, status == expected)?;
    }
    ensure("queueing nothing", read_sizes(b, 16, 2).0 == SHOULD_WAIT)?;
    for h in many.into_iter().chain([event]) {
        // SAFETY: the call takes no pointers.
        check("keeping the handles", unsafe { sys::handle_close(h) })?;
    }

    let mut sent = 0;
    let rights = (1 << 0) | (1 << 1) | (1 << 9);
    // SAFETY: the out pointer is valid for the call.
    check("creating a handle to send", unsafe {
        sys::event_create(0, &mut event)
    })?;
    // SAFETY: the out pointer is valid for the call.
    check("restricting its rights", unsafe {
        sys::handle_replace(event, rights, &mut sent)
    })?;
    // SAFETY: the pointers describe the message and the handle.
    check("writing", unsafe {
        sys::channel_write(a, b"eight by".as_ptr(), 8, &sent, 1)
    })?;
    ensure(
        "moving the handle out of the table",
        // SAFETY: the call takes no pointers.
        unsafe { sys::handle_close(sent) } == BAD_HANDLE,
    )?;
    for (cap, handles_cap) in [(4, 1), (8, 0)] {
        ensure(
            "storing the sizes of a message too large",
            read_sizes(b, cap, handles_cap) == (BUFFER_TOO_SMALL, 8, 1),
        )?;
    }
    let m = read_message(b, 1)?;
    ensure(
        "keeping the message",
        &m.bytes[..m.len] == b"eight by" && m.count == 1,
    )?;
    ensure(
        "keeping the rights",
        u32_at(&info::<0x18>(m.handles[0], 1)?, 20) == rights,
    )?;
    // SAFETY: the calls take no pointers.
    unsafe {
        check(
            "closing the received handle",
            sys::handle_close(m.handles[0]),
        )?;
        check("closing one end", sys::handle_close(a))?;
    }
    ensure(
        "reporting the closed peer",
        read_sizes(b, 16, 2).0 == PEER_CLOSED,
    )?;
    ensure(
        "refusing to write to a closed peer",
        // SAFETY: the pointer describes one byte.
        unsafe { sys::channel_write(b, b"x".as_ptr(), 1, core::ptr::null(), 0) } == PEER_CLOSED,
    )?;
    // SAFETY: the call takes no pointers.
    check("closing the other end", unsafe { sys::handle_close(b) })
}

/// Waits on the word at `addr` until woken, then exits.
extern "sysv64" fn word_waiter(addr: u64, _: u64) -> ! {
    // SAFETY: `addr` is a mapped, aligned word.
    unsafe { sys::futex_wait(addr as *const u32, 0, i64::MAX) };
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// A call whose output lies on a read-only page fails before changing
/// any state: written bytes are not queued, and read bytes and packets
/// stay queued.
fn failed_outputs(s: &Own) -> Result {
    let page = create_memory(PAGE_SIZE)?;
    let mut ro = 0;
    // SAFETY: the out pointer is valid for the call.
    check("mapping read-only", unsafe {
        sys::vm_map(s.process, PERM_READ, 0, page, 0, PAGE_SIZE, &mut ro)
    })?;
    let ro_u64 = ro as *mut u64;

    let (mut a, mut b) = (0, 0);
    // SAFETY: the pointers are valid for the call.
    check("creating a stream", unsafe {
        sys::stream_create(0, &mut a, &mut b)
    })?;
    // SAFETY: the pointers are valid; the count lies on a read-only page.
    let status = unsafe { sys::stream_write(a, b"lost".as_ptr(), 4, ro_u64) };
    ensure("failing a write", status == INVALID_ARGS)?;
    let mut buf = [0u8; 8];
    ensure(
        "queueing nothing",
        stream_io(b, false, &mut buf).0 == SHOULD_WAIT,
    )?;
    ensure("writing", stream_io(a, true, &mut { *b"kept" }) == (0, 4))?;
    // SAFETY: the buffer is valid; the count lies on a read-only page.
    let status = unsafe { sys::stream_read(b, buf.as_mut_ptr(), 8, ro_u64) };
    ensure("failing a read", status == INVALID_ARGS)?;
    let mut actual = 0;
    // SAFETY: the buffer lies on a read-only page; the count is valid.
    let status = unsafe { sys::stream_read(b, ro as *mut u8, 8, &mut actual) };
    ensure(
        "failing a read into read-only bytes",
        status == INVALID_ARGS,
    )?;
    ensure(
        "keeping read bytes",
        stream_io(b, false, &mut buf) == (0, 4) && &buf[..4] == b"kept",
    )?;

    let mut port = 0;
    // SAFETY: the pointer is valid for the call.
    check("creating a port", unsafe { sys::port_create(0, &mut port) })?;
    let mut user = [0u8; 48];
    user[..8].copy_from_slice(&5u64.to_le_bytes());
    // SAFETY: the pointer describes `user`.
    check("queueing", unsafe { sys::port_queue(port, user.as_ptr()) })?;
    // SAFETY: the packet output lies on a read-only page.
    let status = unsafe { sys::port_wait(port, soon(), ro as *mut u8) };
    ensure("failing a wait", status == INVALID_ARGS)?;
    let (status, p) = take_packet(port, soon());
    ensure("keeping the packet", status == 0 && u64_at(&p, 0) == 5)?;

    let word = create_memory(PAGE_SIZE)?;
    let addr = map(s.process, word, PAGE_SIZE)?;
    spawn2(s, word_waiter, addr, 0)?;
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_sleep(now() + 20_000_000) };
    // SAFETY: the word is mapped; the count lies on a read-only page.
    let status = unsafe { sys::futex_wake(addr as *const u32, 1, ro as *mut u32) };
    ensure("failing a wake", status == INVALID_ARGS)?;
    ensure("leaving the waiter waiting", futex_wake(addr, 1)? == 1)?;

    // A blocked real-time signal stays pending through a failed wait.
    let rt = 1u64 << (40 - 1);
    // SAFETY: the calls take no pointers or a null one.
    unsafe {
        check(
            "blocking a signal",
            sys::signal_set_mask(0, rt, core::ptr::null_mut()),
        )?;
        check("sending a signal", sys::thread_signal(s.thread, 40, 77))?;
    }
    // SAFETY: the information output lies on a read-only page.
    let status = unsafe { sys::signal_wait(rt, soon(), ro as *mut u8) };
    ensure("failing a signal wait", status == INVALID_ARGS)?;
    let mut info = [0u8; 64];
    // SAFETY: the pointer describes `info`, which holds a record.
    let status = unsafe { sys::signal_wait(rt, soon(), info.as_mut_ptr()) };
    // SAFETY: the call takes a null pointer.
    unsafe { sys::signal_set_mask(1, rt, core::ptr::null_mut()) };
    ensure(
        "keeping the signal",
        status == 0 && u32_at(&info, 0) == 40 && u64_at(&info, 0x10) == 77,
    )
}

/// Calls refuse `options` with a bit they do not define, and write no
/// output.
fn undefined_options_refused(s: &Own) -> Result {
    let mem = create_memory(PAGE_SIZE)?;
    let (mut pager, mut port) = (0, 0);
    // SAFETY: the out pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
    }
    let mut outs = [Handle::MAX; 2];
    let mut addr = u64::MAX;
    let (out, out1) = (outs.as_mut_ptr(), outs[1..].as_mut_ptr());
    let undefined = 1 << 31;
    // SAFETY: the out pointers are valid for the calls.
    let statuses = unsafe {
        [
            ("creating an event", sys::event_create(1, out)),
            ("creating a timer", sys::timer_create(1, out)),
            ("creating a port", sys::port_create(1, out)),
            ("creating a pager", sys::pager_create(1, out)),
            ("creating a timeline", sys::timeline_create(0, 1, out)),
            ("creating a channel", sys::channel_create(1, out, out1)),
            ("creating a stream", sys::stream_create(1, out, out1)),
            (
                "creating a memory object",
                sys::mem_create(PAGE_SIZE, undefined, out),
            ),
            ("cloning", sys::mem_clone(mem, undefined, 0, PAGE_SIZE, out)),
            (
                "creating a paged object",
                sys::pager_create_memory(pager, port, 0, PAGE_SIZE, undefined, out),
            ),
            (
                "creating an exception channel",
                sys::exception_channel_create(s.process, undefined, out),
            ),
            (
                "mapping",
                sys::vm_map(
                    s.process,
                    PERM_READ | 1 << 3,
                    0,
                    mem,
                    0,
                    PAGE_SIZE,
                    &mut addr,
                ),
            ),
        ]
    };
    for (what, status) in statuses {
        ensure(what, status == INVALID_ARGS)?;
    }
    ensure(
        "writing no output",
        outs == [Handle::MAX; 2] && addr == u64::MAX,
    )?;
    for h in [mem, pager, port] {
        // SAFETY: the call takes no pointers.
        check("closing", unsafe { sys::handle_close(h) })?;
    }
    Ok(())
}

/// `SYSCALL` clears DF, AC and NT, which the debug kernel checks as each
/// system call enters it: this makes one with all three set.
fn syscall_clears_flags(_: &Own) -> Result {
    const FLAGS: u64 = (1 << 10) | (1 << 14) | (1 << 18);
    const HANDLE_CLOSE: i64 = 1;
    let status: i64;
    // SAFETY: closing handle 0 only fails. The block steps over the red
    // zone before it pushes, and clears the flags again before it ends.
    unsafe {
        core::arch::asm!(
            "sub rsp, 128",
            "pushfq",
            "or qword ptr [rsp], {set}",
            "popfq",
            "syscall",
            "pushfq",
            "and qword ptr [rsp], {clear}",
            "popfq",
            "add rsp, 128",
            set = in(reg) FLAGS,
            clear = in(reg) !FLAGS,
            inlateout("rax") HANDLE_CLOSE => status,
            in("rdi") 0u64,
            out("rcx") _,
            out("r11") _,
        )
    };
    // SAFETY: the call takes no pointers.
    let plain = unsafe { sys::handle_close(0) };
    ensure("making the call", status == plain)
}
