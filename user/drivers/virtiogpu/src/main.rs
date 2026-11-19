//! The virtio-gpu driver, for display only: `protocol` writes the
//! commands that it sends and reads the device's responses, and `edid`
//! reads the modes of a connector's EDID. Both are built for the host
//! with their tests.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

mod edid;
mod protocol;

#[cfg(not(target_os = "mikros"))]
fn main() {}
