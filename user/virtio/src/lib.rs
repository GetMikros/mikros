//! The virtio transport over PCI that every virtio driver uses: finding
//! and mapping a modern device's configuration structures, initializing
//! it, negotiating features, setting up split virtqueues, notifying the
//! device and taking its interrupts.
//!
//! A driver calls `Transport::start` with the bus channel and the `DMA`
//! resource its `Start` gave it, the features it implements and the
//! number of virtqueues it uses, and then submits buffers to the queues.

#![no_std]

extern crate alloc;

mod caps;
mod mmio;
mod queue;
mod transport;

pub use caps::SharedMemory;
pub use mmio::Mmio;
pub use queue::{Buffer, Queue};
pub use transport::{Started, Transport};

use mikros_rt::interfaces::mikros::base::Status;

/// `VIRTIO_F_VERSION_1`, which every device must offer.
pub const F_VERSION_1: u64 = 1 << 32;
/// The features the library never accepts: `VIRTIO_F_ACCESS_PLATFORM`
/// and `VIRTIO_F_RING_PACKED`.
pub const F_ACCESS_PLATFORM: u64 = 1 << 33;
pub const F_RING_PACKED: u64 = 1 << 34;

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
    /// The device does not offer `VIRTIO_F_VERSION_1`.
    NotModern,
    /// The device did not keep `FEATURES_OK`.
    FeaturesRefused,
    /// The device did not finish its reset in time.
    ResetTimedOut,
    /// The device has no such virtqueue, or one of size 0.
    NoQueue(u16),
}

pub(crate) fn kernel(status: i64) -> Result<(), Error> {
    match status {
        mikros_rt::OK => Ok(()),
        status => Err(Error::Kernel(status)),
    }
}
