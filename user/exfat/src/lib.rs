//! Reading exFAT volumes, for the exFAT server: the boot region and its
//! backup, the dirty flag, the up-case table, the allocation bitmap's
//! count of free clusters, the FAT with its chains of clusters,
//! directories of entry sets, and files, whose clusters lie in a chain or
//! in a row without one.
//!
//! The volume lies on a `Device`, as a FAT volume does. Its metadata
//! sectors are cached and written through at once; file data is read
//! from the device directly.

#![no_std]

extern crate alloc;

mod boot;
mod dir;
mod entry;
mod files;
mod upcase;
mod volume;

pub use boot::Layout;
pub use dir::{Dir, Node};
pub use entry::{ARCHIVE, DIRECTORY, NAME_MAX, Stamp};
pub use mikros_fat::{Device, Error};
pub use volume::Volume;

#[cfg(test)]
mod tests;
