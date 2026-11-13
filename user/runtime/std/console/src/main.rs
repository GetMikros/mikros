//! The text console: `program` draws the virtual consoles on the display,
//! reads the keyboard, and runs the logins and sessions of each console;
//! `emulator` parses what a terminal's programs write and keeps its screen,
//! and encodes the keys it is sent. Only `emulator`, with its tests, is
//! built for the host.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]
#![cfg_attr(target_os = "mikros", feature(mikros_handles))]

// `emulator` uses only what the alloc library has of std.
extern crate alloc;

mod emulator;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() {
    program::main();
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
