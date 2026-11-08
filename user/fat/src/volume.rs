//! A mounted volume: its directories and files.

use crate::bpb::{FatType, Layout};
use crate::dir::{self, ENTRY, LongName, Short, Slot, Stamp, VOLUME_ID};
use crate::disk::Disk;
use crate::table::{self, Entry};
use crate::{Device, Error};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// The largest file, in bytes.
pub const FILE_MAX: u64 = 0xFFFF_FFFF;
const FS_INFO_LEAD: u32 = 0x4161_5252;
const FS_INFO_STRUCT: u32 = 0x6141_7272;
const FS_INFO_TRAIL: u32 = 0xAA55_0000;

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
    /// Whether the volume is marked as in use only while a change is in
    /// progress, as `begin_change` and `end_change` mark it, rather than
    /// from mounting to `unmount`.
    pub(crate) quick: bool,
    /// The free clusters, once counted, and where to look for one next.
    pub(crate) free: Option<u32>,
    pub(crate) hint: u32,
}

impl<D: Device> Volume<D> {
    /// Mounts the volume on `device`, a partition of `size` bytes, for
    /// writing when `writable`. A volume that was not shut down cleanly
    /// is mounted read-only. Mounting for writing marks the volume as
    /// in use until `unmount`.
    pub fn mount(device: D, size: u64, writable: bool) -> Result<Volume<D>, Error> {
        Self::open(device, size, writable, false)
    }

    /// Mounts the volume as `mount` does, but leaves it marked as shut
    /// down cleanly: `begin_change` and `end_change` mark it as in use
    /// while each change is in progress, so that a device removed between
    /// changes holds a volume that was shut down cleanly.
    pub fn mount_quick_disconnect(
        device: D,
        size: u64,
        writable: bool,
    ) -> Result<Volume<D>, Error> {
        Self::open(device, size, writable, true)
    }

    fn open(device: D, size: u64, writable: bool, quick: bool) -> Result<Volume<D>, Error> {
        let mut disk = Disk::new(device, 512);
        let mut boot = [0u8; 512];
        disk.read(0, &mut boot)?;
        let layout = Layout::parse(&boot, size)?;
        let mut disk = Disk::new(disk.device, layout.sector_size);
        let writable = writable && table::clean(&mut disk, &layout)?;
        let mut volume = Volume {
            disk,
            layout,
            writable,
            quick,
            free: None,
            hint: 2,
        };
        if writable && !quick {
            table::set_clean(&mut volume.disk, &volume.layout, false)?;
            volume.disk.device.flush()?;
        }
        Ok(volume)
    }

    /// Marks a volume mounted with `mount_quick_disconnect` for writing
    /// as in use, on stable storage, before a change.
    pub fn begin_change(&mut self) -> Result<(), Error> {
        if !self.writable || !self.quick {
            return Ok(());
        }
        table::set_clean(&mut self.disk, &self.layout, false)?;
        self.disk.device.flush()
    }

    /// Flushes the device after a change, and marks a volume mounted with
    /// `mount_quick_disconnect` for writing as shut down cleanly again, on
    /// stable storage.
    pub fn end_change(&mut self) -> Result<(), Error> {
        self.disk.device.flush()?;
        if !self.writable || !self.quick {
            return Ok(());
        }
        table::set_clean(&mut self.disk, &self.layout, true)?;
        self.disk.device.flush()
    }

    /// Writes the free cluster count, marks the volume as shut down
    /// cleanly and flushes the device, when it is mounted for writing.
    /// It stays mounted read-only.
    pub fn unmount(&mut self) -> Result<(), Error> {
        if !self.writable {
            return Ok(());
        }
        self.write_fs_info()?;
        self.disk.device.flush()?;
        table::set_clean(&mut self.disk, &self.layout, true)?;
        self.disk.device.flush()?;
        self.writable = false;
        Ok(())
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

    /// Makes everything written durable.
    pub fn sync(&mut self) -> Result<(), Error> {
        if self.writable {
            self.write_fs_info()?;
        }
        self.disk.device.flush()
    }

    /// How many clusters are free.
    pub fn free_clusters(&mut self) -> Result<u32, Error> {
        if let Some(free) = self.free {
            return Ok(free);
        }
        let mut free = 0;
        for cluster in 2..self.layout.clusters + 2 {
            if table::get(&mut self.disk, &self.layout, cluster)? == Entry::Free {
                free += 1;
            }
        }
        self.free = Some(free);
        Ok(free)
    }

    fn write_fs_info(&mut self) -> Result<(), Error> {
        let Some(at) = self.layout.fs_info else {
            return Ok(());
        };
        let free = self.free_clusters()?;
        let mut sector = [0u8; 512];
        self.disk.read(at, &mut sector)?;
        let u32_at = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        if u32_at(&sector, 0) != FS_INFO_LEAD
            || u32_at(&sector, 484) != FS_INFO_STRUCT
            || u32_at(&sector, 508) != FS_INFO_TRAIL
        {
            return Ok(());
        }
        let mut values = [0u8; 8];
        values[..4].copy_from_slice(&free.to_le_bytes());
        values[4..].copy_from_slice(&self.hint.to_le_bytes());
        self.disk.write(at + 488, &values)
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

    /// Where the short entry of `node` lies on the volume, in bytes.
    pub fn entry_offset(&mut self, node: &Node) -> Result<u64, Error> {
        self.slot_at(node.parent, node.slot)?.ok_or(Error::Io)
    }

    /// How many bytes the clusters of `dir` take: those of its chain, and
    /// none for the fixed root directory.
    pub fn allocated_dir(&mut self, dir: Dir) -> Result<u64, Error> {
        match dir {
            Dir::Root => Ok(0),
            Dir::Cluster(first) => self.allocated(first),
        }
    }

    /// How many bytes the chain of clusters that starts at `first` takes,
    /// 0 for none.
    pub fn allocated(&mut self, first: u32) -> Result<u64, Error> {
        if first == 0 {
            return Ok(0);
        }
        let (mut count, mut cluster) = (1u64, first);
        while let Some(next) = table::next(&mut self.disk, &self.layout, cluster)? {
            cluster = next;
            count += 1;
            if count > u64::from(self.layout.clusters) {
                return Err(Error::Io);
            }
        }
        Ok(count * u64::from(self.layout.cluster_size))
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
