//! A test program for exception handling and thread events. With a
//! second argument of 0 it executes an undefined instruction at once;
//! otherwise it waits on its bootstrap channel until it is killed.

#![no_std]
#![no_main]

use mikros_rt::sys::{self, Handle};

mikros_rt::raw_main!(main);

fn main(bootstrap: Handle, arg2: u64) -> i64 {
    if arg2 == 0 {
        // SAFETY: the instruction only raises an exception.
        unsafe { core::arch::asm!("ud2") };
    }
    let mut observed = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::object_wait_one(bootstrap, 1 << 0, i64::MAX, &mut observed) };
    0
}
