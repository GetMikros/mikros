//! HID for input drivers, independent of the transport: parsing report
//! descriptors, and turning input reports into evdev events.

#![no_std]

extern crate alloc;

pub mod descriptor;
#[cfg(test)]
mod descriptor_tests;
