//! A test program that answers calls on its bootstrap channel: each
//! reply carries the request's transaction ID and the rest of its bytes,
//! and the request's handles. A request of `late` is answered after
//! 50 ms, `small` gets a reply larger than any test buffer, and `quit`
//! ends the program without a reply.

#![no_std]
#![no_main]

use mikros_rt::sys::{self, Handle};
use mikros_test::{Result, check, read_message, sleep_ms};

mikros_rt::raw_main!(main);

fn serve(channel: Handle) -> Result {
    loop {
        let request = read_message(channel, 10_000)?;
        let body = &request.bytes[4..request.len];
        match body {
            b"quit" => return Ok(()),
            b"late" => sleep_ms(50),
            _ => {}
        }
        let mut reply = [0u8; 256];
        let len = if body == b"small" { 200 } else { request.len };
        reply[..request.len].copy_from_slice(&request.bytes[..request.len]);
        // SAFETY: the pointers and lengths describe `reply` and the
        // request's handles.
        let status = unsafe {
            sys::channel_write(
                channel,
                reply.as_ptr(),
                len as u64,
                request.handles.as_ptr(),
                request.count as u64,
            )
        };
        check("replying", status)?;
    }
}

fn main(bootstrap: Handle, _arg2: u64) -> i64 {
    match serve(bootstrap) {
        Ok(()) => 0,
        Err(failure) => failure.status,
    }
}
