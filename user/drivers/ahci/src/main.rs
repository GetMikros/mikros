//! The AHCI driver: `driver` serves SATA disks through the HBA's ports,
//! with the ATA commands that `ata` builds and the answers it parses. Only
//! `ata`, with its tests, is built for the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

mod ata;
#[cfg(target_os = "mikros")]
mod driver;

#[cfg(not(target_os = "mikros"))]
fn main() {}
