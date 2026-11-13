//! The CMOS real-time clock driver: `driver` serves the clock, which
//! `clock` reads and sets through its ports, in the register format that
//! `cmos` decodes and encodes. Only `cmos`, with its tests, is built for
//! the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
extern crate alloc;

mod cmos;
#[cfg(target_os = "mikros")]
mod driver;

#[cfg(not(target_os = "mikros"))]
fn main() {}
