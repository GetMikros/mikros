//! The architecture-independent part of the Mikros kernel. It builds for
//! the development host, so that its code can be tested there.

#![cfg_attr(not(test), no_std)]

pub mod bootinfo;
pub mod panic;
