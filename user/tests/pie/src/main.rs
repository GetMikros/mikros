//! A position-independent program for the loader tests. Its exit code
//! depends on data that only relocation makes valid: a table of
//! references and a function pointer.

#![no_std]
#![no_main]

use mikros_rt::sys;

mikros_rt::raw_main!(main);

static WORDS: [&str; 3] = ["one", "three", "seven"];
static PICK: fn(&[&str]) -> usize = |w| w[1].len() + w[2].len();

/// Exits with 10 when the relocated data reads as written, and with 1
/// otherwise.
fn main(bootstrap: sys::Handle, _: u64) -> i64 {
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(bootstrap) };
    let stack = [0u8; 64 * 1024];
    let used = core::hint::black_box(&stack).len();
    if WORDS[0] == "one" && PICK(&WORDS) == 10 && used == 64 * 1024 {
        10
    } else {
        1
    }
}
