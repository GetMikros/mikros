//! HID for input drivers, independent of the transport: parsing report
//! descriptors, and turning input reports into evdev events.

pub mod descriptor;
#[cfg(test)]
mod descriptor_tests;
pub mod mapping;
#[cfg(test)]
mod mapping_tests;
