//! The USB HID driver: `driver` reads the reports of a keyboard or mouse
//! interface through `usb` and publishes their input events, which `hid`
//! maps from the report descriptor. Only `hid`, with its tests, is built
//! for the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

#[cfg(target_os = "mikros")]
mod driver;
mod hid;
#[cfg(target_os = "mikros")]
mod usb;

#[cfg(not(target_os = "mikros"))]
fn main() {}
