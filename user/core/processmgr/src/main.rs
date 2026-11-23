//! The process manager. `program` serves what init and the other
//! processes ask for; only what needs no Mikros is built for the host,
//! with its tests.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

// Served by `program` once it is wired in.
#[allow(dead_code)]
mod gate;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(not(target_os = "mikros"))]
fn main() {}
