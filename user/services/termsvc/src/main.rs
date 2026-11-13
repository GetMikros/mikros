//! The terminal service: `service` makes terminals and serves both of
//! their ends, with job control; `terminal` holds a terminal's termios
//! settings and its line discipline. Only `terminal`, with its tests, is
//! built for the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

#[cfg(target_os = "mikros")]
mod service;
mod terminal;

#[cfg(not(target_os = "mikros"))]
fn main() {}
