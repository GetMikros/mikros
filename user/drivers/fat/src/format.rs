//! What the server needs of the volumes of the format it serves.

use crate::device::BlockDevice;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_fat::{Error, Local};

/// A mounted volume of the format that a server serves, on a
/// `BlockDevice`, with its directories and nodes.
pub trait Format: Sized + 'static {
    /// A directory of the volume.
    type Dir: Copy + Eq;
    /// A file or directory, as the entry of a directory.
    type Node: Clone;
    /// A timestamp as the volume stores it.
    type Stamp: Copy;

    /// The server's name, for its log messages.
    const NAME: &'static str;
    /// The largest size of a file.
    const FILE_MAX: u64;

    /// Mounts the volume on `device`, a partition of `size` bytes, for
    /// writing when `writable`. With `quick`, the volume is marked as in
    /// use only while a change is in progress.
    fn mount(device: BlockDevice, size: u64, writable: bool, quick: bool) -> Result<Self, Error>;
    fn begin_change(&mut self) -> Result<(), Error>;
    fn end_change(&mut self) -> Result<(), Error>;
    fn unmount(&mut self) -> Result<(), Error>;
    fn sync(&mut self) -> Result<(), Error>;
    fn read_only(&self) -> bool;
    fn root(&self) -> Self::Dir;
    /// The type of the file system that `QueryFilesystem` reports.
    fn kind(&self) -> &'static str;
    fn cluster_size(&self) -> u32;
    /// How many clusters the volume has.
    fn clusters(&self) -> u32;
    fn free_clusters(&mut self) -> Result<u32, Error>;
    fn label(&mut self) -> Result<Option<String>, Error>;

    fn list(
        &mut self,
        dir: Self::Dir,
        from: u32,
        max: usize,
    ) -> Result<(Vec<Self::Node>, u32), Error>;
    fn find(&mut self, dir: Self::Dir, name: &str) -> Result<Self::Node, Error>;
    fn read(&mut self, node: &mut Self::Node, offset: u64, buf: &mut [u8]) -> Result<usize, Error>;
    fn write(
        &mut self,
        node: &mut Self::Node,
        offset: u64,
        data: &[u8],
        now: Self::Stamp,
    ) -> Result<(), Error>;
    fn truncate(&mut self, node: &mut Self::Node, size: u64, now: Self::Stamp)
    -> Result<(), Error>;
    fn set_times(
        &mut self,
        node: &mut Self::Node,
        accessed: Option<Self::Stamp>,
        modified: Option<Self::Stamp>,
    ) -> Result<(), Error>;
    fn create(
        &mut self,
        dir: Self::Dir,
        name: &str,
        directory: bool,
        now: Self::Stamp,
    ) -> Result<Self::Node, Error>;
    fn remove(&mut self, dir: Self::Dir, name: &str, directory: bool) -> Result<(), Error>;
    fn rename(
        &mut self,
        from: Self::Dir,
        name: &str,
        to: Self::Dir,
        new_name: &str,
    ) -> Result<Self::Node, Error>;
    /// The bytes that the clusters of `dir` take.
    fn allocated_dir(&mut self, dir: Self::Dir) -> Result<u64, Error>;
    /// The bytes that the clusters of `node` take.
    fn allocated(&mut self, node: &Self::Node) -> Result<u64, Error>;
    /// Where the entry of `node` lies on the volume, in bytes.
    fn entry_offset(&mut self, node: &Self::Node) -> Result<u64, Error>;
    /// The directory that the node `node` is, when it is one.
    fn as_dir(&self, node: &Self::Node) -> Option<Self::Dir>;

    /// The name of `node` as the volume gives it.
    fn name(node: &Self::Node) -> &str;
    /// The directory that holds the entry of `node`, and the entry's slot
    /// there, which together name the entry.
    fn place(node: &Self::Node) -> (Self::Dir, u32);
    fn is_directory(node: &Self::Node) -> bool;
    fn size(node: &Self::Node) -> u64;
    /// When `node` was created, modified and accessed, in UTC
    /// nanoseconds since 1970, with local times in `local`.
    fn times(node: &Self::Node, local: &Local) -> [i64; 3];
    /// The stamp that the server writes for the UTC time `utc`, in the
    /// local time of `local`.
    fn stamp(utc: i64, local: &Local) -> Self::Stamp;
}
