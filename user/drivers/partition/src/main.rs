//! The partition driver: `driver` reads a disk's GPT or MBR partition
//! table, which `gpt` parses, and serves a block device for each
//! partition. Only `gpt`, with its tests, is built for the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

#[cfg(target_os = "mikros")]
mod driver;
mod gpt;
#[cfg(target_os = "mikros")]
mod link;
#[cfg(target_os = "mikros")]
mod partition;
#[cfg(target_os = "mikros")]
mod service;
#[cfg(target_os = "mikros")]
mod table;
#[cfg(target_os = "mikros")]
mod writers;

#[cfg(not(target_os = "mikros"))]
fn main() {}
