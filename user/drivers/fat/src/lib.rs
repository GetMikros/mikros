//! The server of the FAT server's volumes, generic over the format of the
//! volumes it serves, which `Format` describes: it serves the `Driver`
//! channel, mounts the volume on the partition, and serves its files
//! through `mikros.fs.Filesystem` (see `server`).

#![no_std]

extern crate alloc;

mod device;
mod driver;
mod format;
mod fs;
mod memory;
mod node;
mod server;
pub mod time;

pub use device::BlockDevice;
pub use format::Format;
pub use server::run;
