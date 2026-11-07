//! What storage drivers share: serving `Block` for a disk on a
//! connection, serving the disk's raw access service, and the pinned
//! memory that buffers and requests use.
//!
//! A driver implements `Disk` for each disk it drives, and calls `serve`
//! for each `Block` channel and `serve_provider` for the provider it
//! publishes with the class `storage`.

#![no_std]

extern crate alloc;

mod block;
mod dma;
mod memory;
mod mmio;
mod service;

pub use block::serve;
pub use dma::Contiguous;
pub use memory::{PAGE, pages, pinned, pinned_with};
pub use mmio::Mmio;
pub use service::serve_provider;

/// One millisecond, in nanoseconds of the monotonic clock.
pub const MS: i64 = 1_000_000;

/// The monotonic clock, in nanoseconds.
pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { mikros_rt::sys::clock_get_monotonic(&mut now) };
    now
}

use core::future::Future;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::storage::BlockInfo;

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
