//! A mounted volume: its directories.

use crate::bpb::{FatType, Layout};
use crate::dir::{self, ENTRY, Short, Slot, Stamp, VOLUME_ID};
use crate::disk::Disk;
use crate::table;
use crate::{Device, Error};
use alloc::string::String;
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
        let mut slot = from;
        while nodes.len() < max {
            let Some(at) = self.slot_at(dir, slot)? else {
                break;
            };
            match Slot::parse(&self.read_slot(at)?) {
                Slot::End => break,
                Slot::Free | Slot::Long => {}
                Slot::Short(short) => {
                    let label = short.attributes & VOLUME_ID != 0;
                    let dots = short.name[0] == b'.';
                    if !label && !dots {
                        let name = short.display();
                        nodes.push(Node {
                            name,
                            parent: dir,
                            slot,
                            first_slot: slot,
                            entry: short,
                        });
                    }
                }
            }
            slot += 1;
        }
        Ok((nodes, slot))
    }

    /// The entry of `dir` named `name`, without regard to case.
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
}
