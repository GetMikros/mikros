//! The virtio transport over PCI that every virtio driver uses. So far:
//! finding a modern device's configuration structures through its
//! virtio PCI capabilities, and mapping them.

#![no_std]

extern crate alloc;

mod caps;
mod mmio;

pub use caps::{SharedMemory, Structures, find};
pub use mmio::Mmio;

use mikros_rt::interfaces::mikros::base::Status;

/// Why a device could not be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The bus driver failed a request with this status.
    Bus(Status),
    /// The bus channel failed.
    Channel,
    /// A system call failed with this status.
    Kernel(i64),
    /// The device has no capability for this configuration structure.
    MissingStructure(u8),
}

pub(crate) fn kernel(status: i64) -> Result<(), Error> {
    match status {
        mikros_rt::OK => Ok(()),
        status => Err(Error::Kernel(status)),
    }
}
