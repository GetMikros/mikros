//! The temporary file system: `service` serves the files and folders of
//! `memfs`, which keeps them in memory, with their budgets and watches.
//! Only `memfs`, with its tests, is built for the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

mod memfs;
#[cfg(target_os = "mikros")]
mod service;

#[cfg(not(target_os = "mikros"))]
fn main() {}
