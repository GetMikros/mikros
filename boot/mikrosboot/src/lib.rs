//! The parts of the bootloader that do not depend on UEFI, so that they can
//! be tested on the host.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod attempts;
pub mod bootlist;
pub mod btrfs;
pub mod cmdline;
pub mod cpu;
pub mod crc;
pub mod disk;
pub mod display;
pub mod elf;
pub mod exit;
pub mod firmware;
pub mod gpt;
pub mod handoff;
pub mod info;
pub mod load;
pub mod memmap;
pub mod paging;
pub mod placement;
pub mod report;
pub mod runtime;
pub mod version;

#[cfg(test)]
mod absent;
