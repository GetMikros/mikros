//! Reading and writing FAT12, FAT16 and FAT32 volumes with long file
//! names, for the FAT server: the boot sector, the FAT, directories with
//! their long and short names, and files.
//!
//! The volume lies on a `Device`. Metadata sectors are cached and
//! written through at once, so that the device always holds what the
//! volume has done; `Device::flush` makes it durable.

#![no_std]

extern crate alloc;

mod bpb;
mod dir;
mod disk;
mod files;
mod table;
mod volume;

pub use bpb::{FatType, Layout};
pub use dir::{ARCHIVE, DIRECTORY, HIDDEN, READ_ONLY, SYSTEM, Stamp};
pub use volume::{Dir, FILE_MAX, Node, Volume};

/// Why an operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The device failed, or the volume's structures are damaged.
    Io,
    /// Not a valid FAT volume, or arguments the volume cannot take.
    Invalid,
    NotFound,
    Exists,
    NotDirectory,
    IsDirectory,
    NotEmpty,
    NoSpace,
    ReadOnly,
    NameTooLong,
}

/// What a volume lies on. Offsets and lengths are multiples of 512.
pub trait Device {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), Error>;
    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), Error>;
    /// Makes what was written durable.
    fn flush(&mut self) -> Result<(), Error>;
}

#[cfg(test)]
mod tests;
