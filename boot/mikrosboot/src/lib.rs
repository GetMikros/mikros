//! The parts of the bootloader that do not depend on UEFI, so that they can
//! be tested on the host.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod cmdline;
pub mod display;
pub mod elf;
pub mod firmware;
pub mod info;
pub mod load;
pub mod memmap;
pub mod paging;
pub mod report;
