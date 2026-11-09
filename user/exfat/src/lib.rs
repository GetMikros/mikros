//! Mounting exFAT volumes, for the exFAT server: the boot region and its
//! backup, the dirty flag, the up-case table's checksum, the allocation
//! bitmap's count of free clusters, and the volume label.
//!
//! The volume lies on a `Device`, as a FAT volume does. Its metadata
//! sectors are cached and written through at once.

#![no_std]

extern crate alloc;

mod boot;
mod entry;
mod upcase;
mod volume;

pub use boot::Layout;
pub use mikros_fat::{Device, Error};
pub use volume::Volume;

#[cfg(test)]
mod tests;
