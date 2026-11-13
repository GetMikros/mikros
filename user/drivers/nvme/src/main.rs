//! The NVMe driver: `driver` serves the controller's namespaces through
//! its queues, with the commands and submission queue entries that
//! `command` makes. Only `command`, with its tests, is built for the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

mod command;
#[cfg(target_os = "mikros")]
mod controller;
#[cfg(target_os = "mikros")]
mod driver;
#[cfg(target_os = "mikros")]
mod namespace;
#[cfg(target_os = "mikros")]
mod queue;
#[cfg(target_os = "mikros")]
mod regs;

#[cfg(not(target_os = "mikros"))]
fn main() {}
