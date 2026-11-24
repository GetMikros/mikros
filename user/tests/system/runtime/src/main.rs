//! The runtime library's checks, as a system test: mikros-rt's heap, the
//! MIDL bindings it generates, which a test library here is generated
//! with, and its executor.

#![no_std]
#![no_main]

extern crate alloc;

mod executor;
#[allow(clippy::all, unused)]
mod midl {
    include!(concat!(env!("OUT_DIR"), "/midl.rs"));
}

use mikros_rt::Managed;
use mikros_rt::sys::{self, Handle};
use mikros_test::{
    Own, PAGE_SIZE, Result, TERMINATED, check, create_memory, ensure, in_time, spawn, wait,
};

mikros_rt::managed_main!(main);

const ACCESS_DENIED: i64 = -13;

fn main(m: &Managed) -> i64 {
    mikros_test::run(
        m,
        &[
            ("heap_allocates_and_frees", heap_allocates_and_frees),
            ("midl_bindings_on_channels", midl_bindings_on_channels),
            (
                "channel_read_takes_every_size",
                channel_read_takes_every_size,
            ),
            (
                "executor_waits_on_objects",
                executor::executor_waits_on_objects,
            ),
        ],
    )
}

/// The global allocator serves small and large blocks of any common
/// alignment, keeps their contents, and reuses freed memory.
fn heap_allocates_and_frees(_: &Own) -> Result {
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    #[repr(align(256))]
    struct Aligned([u8; 300]);
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    for (i, size) in [1, 15, 16, 17, 100, 2048, 2049, 5000, 1 << 20]
        .into_iter()
        .enumerate()
    {
        let mut v = Vec::with_capacity(size);
        v.resize(size, i as u8 + 1);
        blocks.push(v);
    }
    for (i, v) in blocks.iter().enumerate() {
        ensure(
            "keeping block contents",
            v.iter().all(|&b| b == i as u8 + 1),
        )?;
    }
    let aligned = Box::new(Aligned([7; 300]));
    ensure(
        "aligning blocks",
        (&*aligned as *const Aligned as usize).is_multiple_of(256),
    )?;
    ensure(
        "keeping aligned contents",
        aligned.0.iter().all(|&b| b == 7),
    )?;
    drop(blocks);
    // The optimizer may not drop the first block, or hand its memory to
    // the second without the allocator.
    let first = core::hint::black_box(Box::new([0u8; 64]));
    let at = &*first as *const [u8; 64] as usize;
    drop(first);
    let again = core::hint::black_box(Box::new([1u8; 64]));
    ensure(
        "reusing a freed block",
        &*again as *const [u8; 64] as usize == at,
    )?;
    // A freed block of a few pages stays mapped for the next of its size,
    // which is still zero when it is asked for as zeros.
    let mut big = core::hint::black_box(Vec::<u8>::with_capacity(5000));
    big.resize(5000, 0xff);
    let at = big.as_ptr() as usize;
    drop(big);
    let zeros = core::hint::black_box(alloc::vec![0u8; 6000]);
    ensure("reusing a freed large block", zeros.as_ptr() as usize == at)?;
    ensure("clearing a reused block", zeros.iter().all(|&b| b == 0))?;
    // Blocks around the limits of the size classes keep their contents
    // and can be freed and asked for again, as can one that is aligned to
    // a page and one that no mapping can be aligned for.
    {
        use alloc::alloc::{alloc, dealloc};
        use core::alloc::Layout;
        for (size, align) in [
            (2048, 8),
            (2049, 8),
            (4096, 8),
            (4097, 8),
            (65536, 8),
            (65537, 8),
            (16, 4096),
            (5000, 4096),
        ] {
            let layout =
                Layout::from_size_align(size, align).map_err(|_| mikros_test::Failure {
                    what: "making a layout",
                    status: 0,
                })?;
            for _ in 0..2 {
                // SAFETY: the layout has a size above zero.
                let block = unsafe { alloc(layout) };
                ensure("allocating around a size class", !block.is_null())?;
                ensure(
                    "aligning around a size class",
                    (block as usize).is_multiple_of(align),
                )?;
                // SAFETY: the block has `size` bytes.
                unsafe { block.write_bytes(0xa5, size) };
                // SAFETY: the block came from `alloc` with this layout.
                unsafe { dealloc(block, layout) };
            }
        }
        let layout = Layout::from_size_align(64, 8192).map_err(|_| mikros_test::Failure {
            what: "making a layout",
            status: 0,
        })?;
        // The optimizer takes an allocation that is only compared with
        // null for one that succeeds, unless the pointer is hidden.
        // SAFETY: the layout has a size above zero.
        let refused = core::hint::black_box(unsafe { alloc(layout) });
        ensure("refusing an alignment above a page", refused.is_null())?;
        // More freed blocks of one class than are kept: the rest is
        // unmapped, and every block is still served.
        let layout = Layout::from_size_align(16 * 1024, 8).map_err(|_| mikros_test::Failure {
            what: "making a layout",
            status: 0,
        })?;
        let mut held = Vec::new();
        for _ in 0..32 {
            // SAFETY: the layout has a size above zero.
            let block = unsafe { alloc(layout) };
            ensure("allocating many blocks of a class", !block.is_null())?;
            held.push(block);
        }
        for block in held {
            // SAFETY: the block came from `alloc` with this layout.
            unsafe { dealloc(block, layout) };
        }
    }
    // Far more than the static arena, in blocks that are each freed.
    for round in 0..64 {
        let big: Vec<u64> = (0..64 * 1024).map(|n| n ^ round).collect();
        ensure("filling a large block", big[4095] == 4095 ^ round)?;
    }
    Ok(())
}

/// Serves `Echo` on the server end `raw` until the client closes it.
extern "sysv64" fn echo_server(raw: u64, _: u64) -> ! {
    use midl::__midl::{OwnedHandle, ServerEnd};
    use midl::test::kernel::*;
    // SAFETY: the test hands this thread the server end.
    let server = EchoServer::new(ServerEnd::new(unsafe {
        OwnedHandle::from_raw(raw as Handle)
    }));
    while let Ok(request) = server.next(i64::MAX) {
        if let EchoRequest::Twice(r, responder) = request {
            let _ = responder.reply(&server, EchoTwiceResponse { value: r.value * 2 });
        }
    }
    drop(server);
    // SAFETY: thread_exit takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Generated MIDL bindings send one-way requests with handles, reduce
/// handle rights, deliver events, and call a server on another thread.
fn midl_bindings_on_channels(s: &Own) -> Result {
    use midl::__midl::{self as rt, OwnedHandle};
    use midl::test::kernel::*;
    let (client_end, server_end) = rt::endpoints::<Echo>().map_err(|_| mikros_test::Failure {
        what: "creating endpoints",
        status: 0,
    })?;
    let client = EchoClient::new(client_end);
    let server = EchoServer::new(server_end);
    let mem = create_memory(PAGE_SIZE)?;
    // SAFETY: the handle is new, and `memory` is its only owner.
    let memory = unsafe { OwnedHandle::from_raw(mem) };
    let note = Note {
        text: "over a channel".into(),
        memory,
    };
    ensure("sending a note", client.send(note).is_ok())?;
    let request = server.try_next();
    let Ok(Some(EchoRequest::Send(r))) = request else {
        return ensure("receiving the note", false);
    };
    ensure("keeping the text", r.note.text == "over a channel")?;
    // Only READ is left: reading works, and writing or inspecting fails.
    let mut byte = [0u8; 1];
    let raw = r.note.memory.raw();
    // SAFETY: the pointers describe `byte`.
    check("reading the memory", unsafe {
        sys::mem_read(raw, 0, byte.as_mut_ptr(), 1)
    })?;
    // SAFETY: the pointers describe `byte`.
    let status = unsafe { sys::mem_write(raw, 0, byte.as_ptr(), 1) };
    ensure("reducing the rights to READ", status == ACCESS_DENIED)?;
    ensure(
        "dropping INSPECT",
        mikros_rt::midl::handle_info(raw).is_none(),
    )?;
    ensure(
        "sending an event",
        server.send_sent(r.note.text.clone(), PAGE_SIZE).is_ok(),
    )?;
    let event = client.next_event(in_time());
    let ok = matches!(event, Ok(EchoEvent::Sent(ref e)) if e.text == "over a channel" && e.size == PAGE_SIZE);
    ensure("receiving the event", ok)?;
    drop((r, client, server));
    let (client_end, server_end) = rt::endpoints::<Echo>().map_err(|_| mikros_test::Failure {
        what: "creating endpoints",
        status: 0,
    })?;
    let client = EchoClient::new(client_end);
    let raw = server_end.into_handle().into_raw();
    let thread = spawn(s, echo_server, u64::from(raw))?;
    let reply = client.twice(21, in_time());
    ensure(
        "calling another thread",
        matches!(reply, Ok(EchoTwiceResponse { value: 42 })),
    )?;
    drop(client);
    ensure(
        "ending the server",
        wait(thread, TERMINATED, in_time()).0 == 0,
    )?;
    // SAFETY: closing takes no pointers.
    check("closing the thread", unsafe { sys::handle_close(thread) })
}

/// `channel_read` returns messages of every size, up to the largest with
/// all its handles, in order, and reports an empty queue and a closed
/// peer, the same as before it read into kept buffers.
fn channel_read_takes_every_size(_: &Own) -> Result {
    use alloc::vec::Vec;
    use mikros_rt::midl::SHOULD_WAIT;
    let (mut a, mut b) = (0, 0);
    // SAFETY: the pointers are valid for the call.
    check("creating a channel", unsafe {
        sys::channel_create(0, &mut a, &mut b)
    })?;
    let pattern = |len: usize| (0..len).map(|i| (i % 251) as u8).collect::<Vec<u8>>();
    for (len, handles) in [(0usize, 0usize), (1, 0), (2049, 3), (65536, 64)] {
        let bytes = pattern(len);
        let mut sent = Vec::new();
        for _ in 0..handles {
            let mut event = 0;
            // SAFETY: the pointer is valid for the call.
            check("creating an event", unsafe {
                sys::event_create(0, &mut event)
            })?;
            sent.push(event);
        }
        // SAFETY: the pointers and lengths describe the vectors.
        check("writing", unsafe {
            sys::channel_write(a, bytes.as_ptr(), len as u64, sent.as_ptr(), handles as u64)
        })?;
        let Ok((got, got_handles)) = mikros_rt::midl::channel_read(b) else {
            return ensure("reading a message", false);
        };
        ensure("reading the bytes", got == bytes)?;
        ensure("reading the handles", got_handles.len() == handles)?;
        for handle in got_handles {
            // SAFETY: closing takes no pointers.
            unsafe { sys::handle_close(handle) };
        }
    }
    ensure(
        "reading an empty queue",
        mikros_rt::midl::channel_read(b) == Err(SHOULD_WAIT),
    )?;
    // Many reads in a row, each of which must give its slot back.
    for _ in 0..64 {
        ensure(
            "polling an empty queue",
            mikros_rt::midl::channel_read(b) == Err(SHOULD_WAIT),
        )?;
    }
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(a) };
    ensure(
        "reading from a closed peer",
        mikros_rt::midl::channel_read(b).is_err_and(|s| s != SHOULD_WAIT),
    )?;
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(b) };
    Ok(())
}
