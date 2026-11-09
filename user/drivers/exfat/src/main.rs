//! The exFAT server, which serves exFAT volumes with the FAT driver's
//! server.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fat_server::{BlockDevice, Format};
use mikros_exfat::{Dir, Error, Local, Node, Stamp, Volume};
use mikros_rt::sys;

mikros_rt::raw_main!(main);

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    fat_server::run::<Exfat>(bootstrap)
}

/// A mounted exFAT volume.
struct Exfat(Volume<BlockDevice>);

impl Format for Exfat {
    type Dir = Dir;
    type Node = Node;
    type Stamp = Stamp;

    const NAME: &'static str = "exfat";
    const FILE_MAX: u64 = u64::MAX;

    fn mount(device: BlockDevice, size: u64, writable: bool, quick: bool) -> Result<Exfat, Error> {
        let volume = match quick {
            true => Volume::mount_quick_disconnect(device, size, writable),
            false => Volume::mount(device, size, writable),
        };
        volume.map(Exfat)
    }

    fn begin_change(&mut self) -> Result<(), Error> {
        self.0.begin_change()
    }

    fn end_change(&mut self) -> Result<(), Error> {
        self.0.end_change()
    }

    fn unmount(&mut self) -> Result<(), Error> {
        self.0.unmount()
    }

    fn sync(&mut self) -> Result<(), Error> {
        self.0.sync()
    }

    fn read_only(&self) -> bool {
        self.0.read_only()
    }

    fn root(&self) -> Dir {
        self.0.root()
    }

    fn kind(&self) -> &'static str {
        "exfat"
    }

    fn cluster_size(&self) -> u32 {
        self.0.layout().cluster_size
    }

    fn clusters(&self) -> u32 {
        self.0.layout().clusters
    }

    fn free_clusters(&mut self) -> Result<u32, Error> {
        Ok(self.0.free_clusters())
    }

    fn label(&mut self) -> Result<Option<String>, Error> {
        self.0.label()
    }

    fn list(&mut self, dir: Dir, from: u32, max: usize) -> Result<(Vec<Node>, u32), Error> {
        self.0.list(dir, from, max)
    }

    fn find(&mut self, dir: Dir, name: &str) -> Result<Node, Error> {
        self.0.find(dir, name)
    }

    fn read(&mut self, node: &mut Node, offset: u64, buf: &mut [u8]) -> Result<usize, Error> {
        self.0.read(node, offset, buf)
    }

    fn write(
        &mut self,
        node: &mut Node,
        offset: u64,
        data: &[u8],
        now: Stamp,
    ) -> Result<(), Error> {
        self.0.write(node, offset, data, now)
    }

    fn truncate(&mut self, node: &mut Node, size: u64, now: Stamp) -> Result<(), Error> {
        self.0.truncate(node, size, now)
    }

    fn set_times(
        &mut self,
        node: &mut Node,
        accessed: Option<Stamp>,
        modified: Option<Stamp>,
    ) -> Result<(), Error> {
        self.0.set_times(node, accessed, modified)
    }

    fn create(&mut self, dir: Dir, name: &str, directory: bool, now: Stamp) -> Result<Node, Error> {
        self.0.create(dir, name, directory, now)
    }

    fn remove(&mut self, dir: Dir, name: &str, directory: bool) -> Result<(), Error> {
        self.0.remove(dir, name, directory)
    }

    fn rename(&mut self, from: Dir, name: &str, to: Dir, new_name: &str) -> Result<Node, Error> {
        self.0.rename(from, name, to, new_name)
    }

    fn allocated_dir(&mut self, dir: Dir) -> Result<u64, Error> {
        self.0.allocated_dir(dir)
    }

    fn allocated(&mut self, node: &Node) -> Result<u64, Error> {
        self.0.allocated(node)
    }

    fn entry_offset(&mut self, node: &Node) -> Result<u64, Error> {
        self.0.entry_offset(node)
    }

    fn as_dir(&self, node: &Node) -> Option<Dir> {
        node.as_dir()
    }

    fn name(node: &Node) -> &str {
        &node.name
    }

    fn place(node: &Node) -> (Dir, u32) {
        (node.parent, node.slot)
    }

    fn is_directory(node: &Node) -> bool {
        node.is_directory()
    }

    fn size(node: &Node) -> u64 {
        node.size()
    }

    fn times(node: &Node, local: &Local) -> [i64; 3] {
        [node.created(), node.modified(), node.accessed()].map(|s| s.to_utc(local))
    }

    fn stamp(utc: i64, local: &Local) -> Stamp {
        Stamp::of_utc(utc, local)
    }
}
