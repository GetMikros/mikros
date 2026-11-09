//! Reading exFAT volumes and resizing their files, for the exFAT server:
//! the boot region and its backup, the dirty flag, the up-case table, the
//! allocation bitmap, the FAT with its chains of clusters, directories of
//! entry sets, and files, whose clusters lie in a row without a chain
//! while the clusters after them are free.
//!
//! The volume lies on a `Device`, as a FAT volume does. Directory and FAT
//! sectors are cached and written through at once; file data is read from
//! the device directly, and the allocation bitmap is held in memory whole.

#![no_std]

extern crate alloc;

mod allocation;
mod bitmap;
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
