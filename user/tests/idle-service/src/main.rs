//! A service that does nothing until it is stopped, which the setup run's
//! root holds for `services stop`, `start` and `restart` to act on
//! without disturbing the session.

#![no_std]
#![no_main]

use mikros_rt::{Managed, sys};

mikros_rt::managed_main!(main);

fn main(_: &Managed) -> i64 {
    loop {
        // SAFETY: the call takes no pointers.
        unsafe { sys::thread_sleep(i64::MAX) };
    }
}
