//! The log service: `service` collects the records of the kernel log and
//! of the processes that log, keeps them in memory and in log files, and
//! serves them; `store` keeps the records and their files' format. Only
//! `store`, with its tests, is built for the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

#[cfg(target_os = "mikros")]
mod service;
mod store;

#[cfg(not(target_os = "mikros"))]
fn main() {}
