//! The PS/2 driver: `driver` serves the i8042 controller's keyboard and
//! mouse through its ports and their interrupts, with the controller,
//! scan code and mouse logic of `ps2`. Only `ps2`, with its tests, is
//! built for the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

#[cfg(target_os = "mikros")]
mod driver;
#[cfg(target_os = "mikros")]
mod hub;
#[cfg(target_os = "mikros")]
mod ports;
mod ps2;

#[cfg(not(target_os = "mikros"))]
fn main() {}
