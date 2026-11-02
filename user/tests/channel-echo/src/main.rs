//! A test program that answers one message on its bootstrap channel: it
//! reads a memory object that came with the message and sends back the
//! object's first bytes after `echo: `, with the handle.

#![no_std]
#![no_main]

use mikros_rt::sys::{self, Handle};
use mikros_test::{Result, check, read_message};

mikros_rt::raw_main!(main);

fn echo(channel: Handle) -> Result {
    let request = read_message(channel, 5000)?;
    let mut reply = [0u8; 64];
    reply[..6].copy_from_slice(b"echo: ");
    let mem = request.handles[0];
    // SAFETY: the pointer and length describe part of `reply`.
    let status = unsafe { sys::mem_read(mem, 0, reply[6..].as_mut_ptr(), 16) };
    check("reading the received memory object", status)?;
    // SAFETY: the pointers and lengths describe `reply` and `request`.
    let status = unsafe {
        sys::channel_write(
            channel,
            reply.as_ptr(),
            22,
            request.handles.as_ptr(),
            request.count as u64,
        )
    };
    check("writing the reply", status)
}

fn main(bootstrap: Handle, _arg2: u64) -> i64 {
    match echo(bootstrap) {
        Ok(()) => 0,
        Err(failure) => failure.status,
    }
}
