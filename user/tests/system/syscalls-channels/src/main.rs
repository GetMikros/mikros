//! The kernel's channels, as a system test: what a channel refuses to
//! send or read, and how it reads into a full handle table.

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
