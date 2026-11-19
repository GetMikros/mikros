//! The virtio-gpu driver, for display only: `driver` drives the device
//! and serves its display; `protocol` writes the commands that it sends
//! and reads the device's responses, and `edid` reads the modes of a
//! connector's EDID. Only `protocol` and `edid`, with their tests, are
//! built for the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

#[cfg(target_os = "mikros")]
mod driver;
mod edid;
mod protocol;

#[cfg(not(target_os = "mikros"))]
fn main() {}
