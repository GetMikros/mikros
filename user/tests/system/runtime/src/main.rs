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
    let first = Box::new([0u8; 64]);
    let at = &*first as *const [u8; 64] as usize;
    drop(first);
    let again = Box::new([1u8; 64]);
    ensure(
        "reusing a freed block",
        &*again as *const [u8; 64] as usize == at,
    )?;
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
