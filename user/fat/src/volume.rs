//! A mounted volume: its directories and files.

use crate::bpb::{FatType, Layout};
use crate::dir::{self, ENTRY, LongName, Short, Slot, Stamp, VOLUME_ID};
use crate::disk::Disk;
use crate::table;
use crate::{Device, Error};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// A directory, by where its entries lie: the fixed root directory of
/// FAT12 and FAT16, or a chain of clusters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dir {
    Root,
    Cluster(u32),
}

/// A directory entry: its name and short entry, and where they lie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    /// The directory that holds the entry, the slot of its short entry
    /// and the slot of its first long-name entry.
    pub parent: Dir,
    pub slot: u32,
    pub first_slot: u32,
    pub(crate) entry: Short,
    /// A cluster of the node's chain and its index, to start from.
    pub(crate) near: (u32, u32),
}

impl Node {
    pub fn is_directory(&self) -> bool {
        self.entry.is_directory()
    }

    /// The directory a directory entry names, with `root` for a cluster
    /// of 0, which `..` entries use for the root.
    pub fn as_dir(&self, root: Dir) -> Option<Dir> {
        match (self.is_directory(), self.entry.cluster) {
            (false, _) => None,
            (true, 0) => Some(root),
            (true, c) => Some(Dir::Cluster(c)),
        }
    }

    pub fn size(&self) -> u64 {
        u64::from(self.entry.size)
    }

    pub fn cluster(&self) -> u32 {
        self.entry.cluster
    }

    pub fn attributes(&self) -> u8 {
        self.entry.attributes
    }

    pub fn created(&self) -> Stamp {
        self.entry.created
    }

    pub fn modified(&self) -> Stamp {
        self.entry.modified
    }

    /// The access date, at midnight.
    pub fn accessed(&self) -> Stamp {
        Stamp {
            date: self.entry.accessed,
            time: 0,
            hundredths: 0,
        }
    }
}

/// A mounted volume on `D`.
pub struct Volume<D> {
    pub(crate) disk: Disk<D>,
    pub(crate) layout: Layout,
    pub(crate) writable: bool,
}

impl<D: Device> Volume<D> {
    /// Mounts the volume on `device`, a partition of `size` bytes.
    /// Mounting for writing is not there yet, and fails with `ReadOnly`.
    pub fn mount(device: D, size: u64, writable: bool) -> Result<Volume<D>, Error> {
        if writable {
            return Err(Error::ReadOnly);
        }
        let mut disk = Disk::new(device, 512);
        let mut boot = [0u8; 512];
        disk.read(0, &mut boot)?;
        let layout = Layout::parse(&boot, size)?;
        let disk = Disk::new(disk.device, layout.sector_size);
        Ok(Volume {
            disk,
            layout,
            writable: false,
        })
    }

    /// The device the volume lies on.
    pub fn device(&mut self) -> &mut D {
        &mut self.disk.device
    }

    /// Gives the device back.
    pub fn into_device(self) -> D {
        self.disk.device
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn read_only(&self) -> bool {
        !self.writable
    }

    /// The root directory.
    pub fn root(&self) -> Dir {
        match self.layout.fat_type {
            FatType::Fat32 => Dir::Cluster(self.layout.root_cluster),
            _ => Dir::Root,
        }
    }

    // Clusters.

    /// The cluster at `index` in the chain that starts at `first`,
    /// starting from `near`, a cluster of the chain and its index, when
    /// that is not past `index`.
    pub(crate) fn cluster_at(
        &mut self,
        first: u32,
        index: u32,
        near: (u32, u32),
    ) -> Result<u32, Error> {
        let (mut cluster, mut at) = match near {
            (c, i) if c >= 2 && i <= index => (c, i),
            _ => (first, 0),
        };
        while at < index {
            cluster = table::next(&mut self.disk, &self.layout, cluster)?.ok_or(Error::Io)?;
            at += 1;
        }
        Ok(cluster)
    }

    // Directory slots.

    /// Where slot `slot` of `dir` lies, or None past the directory's
    /// end.
    pub(crate) fn slot_at(&mut self, dir: Dir, slot: u32) -> Result<Option<u64>, Error> {
        let byte = u64::from(slot) * ENTRY as u64;
        match dir {
            Dir::Root if slot < self.layout.root_entries => Ok(Some(self.layout.root_start + byte)),
            Dir::Root => Ok(None),
            Dir::Cluster(first) => {
                let size = u64::from(self.layout.cluster_size);
                let mut cluster = first;
                for _ in 0..byte / size {
                    match table::next(&mut self.disk, &self.layout, cluster)? {
                        Some(next) => cluster = next,
                        None => return Ok(None),
                    }
                }
                Ok(Some(self.layout.cluster_start(cluster) + byte % size))
            }
        }
    }

    pub(crate) fn read_slot(&mut self, at: u64) -> Result<[u8; ENTRY], Error> {
        let mut b = [0u8; ENTRY];
        self.disk.read(at, &mut b)?;
        Ok(b)
    }

    /// The entries of `dir` from slot `from`, at most `max`, other than
    /// `.`, `..` and the volume label, and the slot to continue from.
    pub fn list(&mut self, dir: Dir, from: u32, max: usize) -> Result<(Vec<Node>, u32), Error> {
        let mut nodes = Vec::new();
        let mut long = LongName::default();
        let mut slot = from;
        while nodes.len() < max {
            let Some(at) = self.slot_at(dir, slot)? else {
                break;
            };
            match Slot::parse(&self.read_slot(at)?) {
                Slot::End => break,
                Slot::Free => long.reset(),
                Slot::Long {
                    order,
                    last,
                    checksum,
                    units,
                } => long.push(slot, order, last, checksum, &units),
                Slot::Short(short) => {
                    let named = long.take(&short);
                    let label = short.attributes & VOLUME_ID != 0;
                    let dots = short.name[0] == b'.';
                    if !label && !dots {
                        let (name, first_slot) = named.unwrap_or_else(|| (short.display(), slot));
                        let near = (short.cluster, 0);
                        nodes.push(Node {
                            name,
                            parent: dir,
                            slot,
                            first_slot,
                            entry: short,
                            near,
                        });
                    }
                }
            }
            slot += 1;
        }
        Ok((nodes, slot))
    }

    /// The entry of `dir` named `name`, by its long or short name,
    /// without regard to case.
    pub fn find(&mut self, dir: Dir, name: &str) -> Result<Node, Error> {
        let mut from = 0;
        loop {
            let (nodes, next) = self.list(dir, from, 64)?;
            if nodes.is_empty() {
                return Err(Error::NotFound);
            }
            for node in nodes {
                if dir::same(&node.name, name) || dir::same(&node.entry.display(), name) {
                    return Ok(node);
                }
            }
            from = next;
        }
    }

    // Files.

    /// Reads up to `buf.len()` bytes of the file `node` from `offset`,
    /// and returns how many it read, 0 at the end.
    pub fn read(&mut self, node: &mut Node, offset: u64, buf: &mut [u8]) -> Result<usize, Error> {
        if node.is_directory() {
            return Err(Error::IsDirectory);
        }
        let size = node.size();
        if offset >= size || buf.is_empty() {
            return Ok(0);
        }
        let len = (size - offset).min(buf.len() as u64) as usize;
        let mut done = 0;
        while done < len {
            let (at, n) = self.extent(node, offset + done as u64, len - done)?;
            self.read_bytes(at, &mut buf[done..done + n])?;
            done += n;
        }
        Ok(len)
    }

    /// Where byte `offset` of the file `node` lies on the device, and how
    /// many of `len` bytes from it lie in the same cluster.
    pub(crate) fn extent(
        &mut self,
        node: &mut Node,
        offset: u64,
        len: usize,
    ) -> Result<(u64, usize), Error> {
        let size = u64::from(self.layout.cluster_size);
        let index = (offset / size) as u32;
        let cluster = self.cluster_at(node.entry.cluster, index, node.near)?;
        node.near = (cluster, index);
        let within = offset % size;
        let n = ((size - within) as usize).min(len);
        Ok((self.layout.cluster_start(cluster) + within, n))
    }

    /// Reads `buf.len()` bytes at `at`, whole sectors straight from the
    /// device.
    fn read_bytes(&mut self, at: u64, buf: &mut [u8]) -> Result<(), Error> {
        let sector = self.disk.sector();
        let start = at - at % sector;
        let end = (at + buf.len() as u64).next_multiple_of(sector);
        if start == at && end == at + buf.len() as u64 {
            return self.disk.read_direct(at, buf);
        }
        let mut whole = vec![0u8; (end - start) as usize];
        self.disk.read_direct(start, &mut whole)?;
        let from = (at - start) as usize;
        buf.copy_from_slice(&whole[from..from + buf.len()]);
        Ok(())
    }

    /// Reads the entry of `node` again, from where it lies.
    pub fn reload(&mut self, node: &mut Node) -> Result<(), Error> {
        let at = self
            .slot_at(node.parent, node.slot)?
            .ok_or(Error::NotFound)?;
        match Slot::parse(&self.read_slot(at)?) {
            Slot::Short(short) => {
                node.entry = short;
                Ok(())
            }
            _ => Err(Error::NotFound),
        }
    }
}
