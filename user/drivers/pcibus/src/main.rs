//! The PCI bus driver: `driver` enumerates the bus through ECAM, adds a
//! device for each function and serves their configuration space, which
//! `pci` decodes. Only `pci`, with its tests, is built for the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

#[cfg(target_os = "mikros")]
mod driver;
mod pci;

#[cfg(not(target_os = "mikros"))]
fn main() {}
