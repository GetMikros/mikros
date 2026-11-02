//! The architecture-independent part of the Mikros kernel. It builds for
//! the development host, so that its code can be tested there.

#![cfg_attr(not(test), no_std)]

pub mod acpi;
pub mod bootinfo;
pub mod clock;
pub mod frames;
pub mod heap;
pub mod log;
pub mod panic;
pub mod random;
pub mod ranges;
pub mod sync;
