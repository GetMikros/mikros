//! What storage drivers share: serving `Block` for a disk on a
//! connection, serving the disk's raw access service, and the pinned
//! memory that buffers and requests use.
//!
//! A driver implements `Disk` for each disk it drives, makes a `Writer`
//! for it, and calls `serve_bus` for each `bus` channel of the disk and
//! `serve_provider` for the provider it publishes with the class
//! `storage`.

#![no_std]

extern crate alloc;

mod block;
mod bus;
mod dma;
mod memory;
mod mmio;
mod service;
mod writer;

pub use block::serve_bus;
pub use bus::PciBus;
pub use dma::Contiguous;
pub use memory::{PAGE, pages, pinned, pinned_with};
pub use mmio::Mmio;
pub use service::serve_provider;
pub use writer::Writer;

/// One millisecond, in nanoseconds of the monotonic clock.
pub const MS: i64 = 1_000_000;

/// The properties a storage controller driver adds a disk device with:
/// `serial`, the disk's serial number from `info`, and `removable`,
/// `true` when `info` reports `REMOVABLE` and `false` otherwise.
pub fn disk_properties(info: &BlockInfo) -> Vec<Property> {
    let removable = info.flags.0 & BlockFlags::REMOVABLE.0 != 0;
    vec![
        Property {
            key: "serial".into(),
            value: info.serial.clone(),
        },
        Property {
            key: "removable".into(),
            value: if removable { "true" } else { "false" }.into(),
        },
    ]
}

/// The monotonic clock, in nanoseconds.
pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { mikros_rt::sys::clock_get_monotonic(&mut now) };
    now
}

use alloc::vec;
use alloc::vec::Vec;
use core::future::Future;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::Property;
use mikros_rt::interfaces::mikros::storage::{BlockFlags, BlockInfo};

/// A disk that a storage driver drives.
pub trait Disk: 'static {
    /// What `GetInfo` reports.
    fn info(&self) -> BlockInfo;
    /// The `DMA` resource from which buffers are made, and the bus
    /// transaction initiator through which they are pinned.
    fn dma(&self) -> &OwnedHandle;
    fn bti(&self) -> &OwnedHandle;
    /// Whether buffers must lie below 4 GiB, for devices that cannot
    /// address memory above it.
    fn below_4g(&self) -> bool {
        false
    }
    /// Reads (`write` false) or writes the blocks from `block` that the
    /// `len` bytes at `offset` in the pages at the physical addresses
    /// `pages` hold, on stable storage when it completes if `fua`. The
    /// request was checked against the disk and the buffer.
    fn transfer(
        &self,
        write: bool,
        block: u64,
        pages: &[u64],
        offset: u64,
        len: u64,
        fua: bool,
    ) -> impl Future<Output = Result<(), Status>>;
    /// Puts every completed write on stable storage.
    fn flush(&self) -> impl Future<Output = Result<(), Status>>;
    /// Discards `count` blocks from `block`, which lie on the disk, on a
    /// disk that reports `DISCARD`.
    fn discard(&self, block: u64, count: u64) -> impl Future<Output = Result<(), Status>>;
}
