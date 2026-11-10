//! Directories: their entry sets, listing them and finding names.

use crate::entry::{self, ENTRY, Set, Stamp};
use crate::volume::Volume;
use crate::{Device, Error};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// A directory: the root, or the directory whose first cluster this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dir {
    Root,
    Sub(u32),
}

/// Where a directory other than the root lies: the directory and slot
/// of its file entry, whether its clusters lie in a row, and its size.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Place {
    pub(crate) parent: Dir,
    pub(crate) slot: u32,
    pub(crate) contiguous: bool,
    pub(crate) size: u64,
}

/// A file or directory: its name, the directory that holds it, the slot
/// of its file entry there, and its entry set.
#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub parent: Dir,
    pub slot: u32,
    pub(crate) set: Set,
    /// A cluster of the node's chain and its index there, to walk on
    /// from.
    pub(crate) near: (u32, u64),
}

impl Node {
    pub fn is_directory(&self) -> bool {
        self.set.is_directory()
    }

    /// The directory that a directory's node names.
    pub fn as_dir(&self) -> Option<Dir> {
        self.is_directory()
            .then_some(Dir::Sub(self.set.first_cluster))
    }

    pub fn size(&self) -> u64 {
        self.set.size
    }

    pub fn attributes(&self) -> u16 {
        self.set.attributes
    }

    pub fn created(&self) -> Stamp {
        self.set.created
    }

    pub fn modified(&self) -> Stamp {
        self.set.modified
    }

    pub fn accessed(&self) -> Stamp {
        self.set.accessed
    }
}

/// The entry sets of a directory whose entries are `bytes`, from slot
/// `slot` on to its end entry: each one's slot and what it holds. A
/// damaged set is left out.
pub(crate) struct Sets<'a> {
    bytes: &'a [u8],
    pub(crate) slot: usize,
}

impl<'a> Sets<'a> {
    pub(crate) fn new(bytes: &'a [u8], slot: usize) -> Sets<'a> {
        Sets { bytes, slot }
    }
}

impl Iterator for Sets<'_> {
    type Item = (u32, Set);

    fn next(&mut self) -> Option<(u32, Set)> {
        loop {
            let at = self.slot.checked_mul(ENTRY)?;
            let e = self.bytes.get(at..at + ENTRY)?;
            match e[0] {
                // The walk stays at the end entry.
                entry::END => return None,
                entry::FILE => {
                    let slot = self.slot;
                    match entry::parse(&self.bytes[at..]) {
                        // The set's own count, which may hold more
                        // secondary entries than its name needs.
                        Some(set) => {
                            self.slot += usize::from(e[1]) + 1;
                            return Some((slot as u32, set));
                        }
                        None => self.slot += 1,
                    }
                }
                _ => self.slot += 1,
            }
        }
    }
}

impl<D: Device> Volume<D> {
    pub fn root(&self) -> Dir {
        Dir::Root
    }

    /// Where `dir`'s entries lie: its first cluster, whether its clusters
    /// lie in a row, and its size, `None` for the root's chain.
    pub(crate) fn dir_extent(&self, dir: Dir) -> Result<(u32, bool, Option<u64>), Error> {
        match dir {
            Dir::Root => Ok((self.layout.root_cluster, false, None)),
            Dir::Sub(first) => {
                let place = self.dirs.get(&first).ok_or(Error::NotFound)?;
                Ok((first, place.contiguous, Some(place.size)))
            }
        }
    }

    /// The bytes of `dir`'s entries, read through the cache.
    pub(crate) fn dir_bytes(&mut self, dir: Dir) -> Result<Vec<u8>, Error> {
        let (first, contiguous, size) = self.dir_extent(dir)?;
        let cluster = u64::from(self.layout.cluster_size);
        let clusters = self.clusters(first, contiguous, size.map(|s| s.div_ceil(cluster)))?;
        self.read_clusters(&clusters, true)
    }

    /// Where entry `slot` of `dir` lies on the volume, in bytes.
    pub(crate) fn entry_at(&mut self, dir: Dir, slot: u32) -> Result<u64, Error> {
        let (first, contiguous, size) = self.dir_extent(dir)?;
        let cluster = u64::from(self.layout.cluster_size);
        let offset = u64::from(slot) * ENTRY as u64;
        if size.is_some_and(|s| offset >= s) {
            return Err(Error::Io);
        }
        let c = self.cluster_at(first, contiguous, offset / cluster, &mut (0, 0))?;
        Ok(self.layout.cluster_start(c) + offset % cluster)
    }

    /// The entry set whose file entry is entry `slot` of `dir`.
    pub(crate) fn set_at(&mut self, dir: Dir, slot: u32) -> Result<Set, Error> {
        let mut file = [0u8; ENTRY];
        let at = self.entry_at(dir, slot)?;
        self.disk.read(at, &mut file)?;
        let count = usize::from(file[1]) + 1;
        let mut set = vec![0u8; count * ENTRY];
        for (i, e) in set.chunks_mut(ENTRY).enumerate() {
            let at = self.entry_at(dir, slot + i as u32)?;
            self.disk.read(at, e)?;
        }
        entry::parse(&set).ok_or(Error::NotFound)
    }

    /// Writes the file entry and stream extension of `node`'s set over
    /// the set where it lies, keeping its name and any other secondary
    /// entries, with the checksum of the whole set again.
    pub(crate) fn store(&mut self, node: &Node) -> Result<(), Error> {
        let mut file = [0u8; ENTRY];
        let first = self.entry_at(node.parent, node.slot)?;
        self.disk.read(first, &mut file)?;
        let count = usize::from(file[1]) + 1;
        if file[0] != entry::FILE || count < 3 {
            return Err(Error::Io);
        }
        let mut places = Vec::with_capacity(count);
        let mut set = vec![0u8; count * ENTRY];
        for (i, e) in set.chunks_mut(ENTRY).enumerate() {
            let at = self.entry_at(node.parent, node.slot + i as u32)?;
            self.disk.read(at, e)?;
            places.push(at);
        }
        node.set.write_fields(&mut set);
        let sum = entry::set_checksum(&set);
        set[2..4].copy_from_slice(&sum.to_le_bytes());
        self.disk.write(places[1], &set[ENTRY..2 * ENTRY])?;
        self.disk.write(places[0], &set[..ENTRY])
    }

    /// The node of `set`, whose file entry is entry `slot` of `dir`. The
    /// place of a directory is kept, for finding its entries.
    pub(crate) fn node(&mut self, dir: Dir, slot: u32, set: Set) -> Node {
        if set.is_directory() {
            let place = Place {
                parent: dir,
                slot,
                contiguous: set.contiguous(),
                size: set.size,
            };
            self.dirs.insert(set.first_cluster, place);
        }
        Node {
            name: set.name(),
            parent: dir,
            slot,
            near: (set.first_cluster, 0),
            set,
        }
    }

    /// The nodes of `dir` from slot `from`, at most `max` of them, and the
    /// slot to continue from.
    pub fn list(&mut self, dir: Dir, from: u32, max: usize) -> Result<(Vec<Node>, u32), Error> {
        let bytes = self.dir_bytes(dir)?;
        let mut sets = Sets::new(&bytes, from as usize);
        let mut nodes = Vec::new();
        while nodes.len() < max {
            let Some((slot, set)) = sets.next() else {
                break;
            };
            nodes.push(self.node(dir, slot, set));
        }
        Ok((nodes, sets.slot as u32))
    }

    /// The node of `dir` named `name`, compared without regard to case
    /// through the up-case table.
    pub fn find(&mut self, dir: Dir, name: &str) -> Result<Node, Error> {
        let units: Vec<u16> = name.encode_utf16().collect();
        if units.len() > entry::NAME_MAX {
            return Err(Error::NameTooLong);
        }
        let bytes = self.dir_bytes(dir)?;
        let found = Sets::new(&bytes, 0).find(|(_, set)| self.upcase.equal(&set.name, &units));
        let (slot, set) = found.ok_or(Error::NotFound)?;
        Ok(self.node(dir, slot, set))
    }

    /// Where the file entry of `node` lies on the volume, in bytes.
    pub fn entry_offset(&mut self, node: &Node) -> Result<u64, Error> {
        self.entry_at(node.parent, node.slot)
    }

    /// The bytes that the clusters of `dir` take.
    pub fn allocated_dir(&mut self, dir: Dir) -> Result<u64, Error> {
        let (first, contiguous, size) = self.dir_extent(dir)?;
        let cluster = u64::from(self.layout.cluster_size);
        let count = self
            .clusters(first, contiguous, size.map(|s| s.div_ceil(cluster)))?
            .len();
        Ok(count as u64 * cluster)
    }

    /// Reads the entry set of `node` again, from where it lies.
    pub fn reload(&mut self, node: &mut Node) -> Result<(), Error> {
        node.set = self.set_at(node.parent, node.slot)?;
        node.name = node.set.name();
        Ok(())
    }
}
