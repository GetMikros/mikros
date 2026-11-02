//! The architecture-independent part of the Mikros kernel. It builds for
//! the development host, so that its code can be tested there.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod acpi;
pub mod bootinfo;
pub mod clock;
pub mod deadline;
pub mod exception;
pub mod frames;
pub mod heap;
pub mod log;
pub mod object;
pub mod panic;
pub mod random;
pub mod ranges;
pub mod regions;
pub mod sched;
pub mod signal;
pub mod status;
pub mod sync;
