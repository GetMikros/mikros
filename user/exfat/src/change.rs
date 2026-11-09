//! Creating, removing and renaming the entries of directories, and
//! growing directories.

use crate::allocation::Extent;
use crate::dir::{Dir, Node, Place};
use crate::entry::{
    self, ALLOCATION_POSSIBLE, ARCHIVE, DIRECTORY, ENTRY, IN_USE, NAME_MAX, Set, Stamp,
};
use crate::volume::Volume;
use crate::{Device, Error};
use alloc::vec::Vec;

/// The largest size of a directory.
const DIR_MAX: u64 = 256 << 20;

impl<D: Device> Volume<D> {
    /// Creates the file, or the directory when `directory`, `name` in
    /// `dir`, created, modified and accessed at `now`.
    pub fn create(
        &mut self,
        dir: Dir,
        name: &str,
        directory: bool,
        now: Stamp,
    ) -> Result<Node, Error> {
        self.check_writable()?;
        let units = check_name(name)?;
        match self.find(dir, name) {
            Ok(_) => return Err(Error::Exists),
            Err(Error::NotFound) => {}
            Err(e) => return Err(e),
        }
        let mut set = Set {
            attributes: if directory { DIRECTORY } else { ARCHIVE },
            created: now,
            modified: now,
            accessed: now,
            flags: ALLOCATION_POSSIBLE,
            valid_size: 0,
            first_cluster: 0,
            size: 0,
            name: units,
        };
        let mut extent = Extent::of(&set, self.layout.cluster_size);
        if directory {
            self.grow(&mut extent, 1, true)?;
            extent.apply(&mut set);
            set.size = u64::from(self.layout.cluster_size);
            set.valid_size = set.size;
        }
        let inserted = self.insert(dir, &set);
        if inserted.is_err() && directory {
            // The directory's cluster goes back when its set could not be
            // written.
            let _ = self.release(&[extent.first]);
        }
        let slot = inserted?;
        Ok(self.node(dir, slot, set))
    }

    /// Removes the entry `name` of `dir`, and frees its clusters. A
    /// directory must be empty, and must be one when `directory`, and a
    /// file when not.
    pub fn remove(&mut self, dir: Dir, name: &str, directory: bool) -> Result<(), Error> {
        self.check_writable()?;
        let node = self.find(dir, name)?;
        match (node.is_directory(), directory) {
            (true, false) => return Err(Error::IsDirectory),
            (false, true) => return Err(Error::NotDirectory),
            _ => {}
        }
        if let Some(sub) = node.as_dir()
            && !self.list(sub, 0, 1)?.0.is_empty()
        {
            return Err(Error::NotEmpty);
        }
        let clusters = self.clusters_of(&node.set)?;
        self.release_set(&node)?;
        if let Some(Dir::Sub(first)) = node.as_dir() {
            self.dirs.remove(&first);
        }
        self.release(&clusters)
    }

    /// Moves the entry `name` of `from` to `new_name` in `to`, replacing
    /// a file there, or an empty directory when it moves a directory.
    pub fn rename(
        &mut self,
        from: Dir,
        name: &str,
        to: Dir,
        new_name: &str,
    ) -> Result<Node, Error> {
        self.check_writable()?;
        let units = check_name(new_name)?;
        let node = self.find(from, name)?;
        let moved = node.as_dir();
        if let Some(moved) = moved
            && self.is_within(to, moved)?
        {
            return Err(Error::Invalid);
        }
        match self.find(to, new_name) {
            Ok(existing) if existing.parent == node.parent && existing.slot == node.slot => {}
            Ok(existing) => {
                match (existing.is_directory(), moved.is_some()) {
                    (true, false) => return Err(Error::IsDirectory),
                    (false, true) => return Err(Error::NotDirectory),
                    _ => {}
                }
                self.remove(to, &existing.name, existing.is_directory())?;
            }
            Err(Error::NotFound) => {}
            Err(e) => return Err(e),
        }
        let mut set = node.set.clone();
        set.name = units;
        // The new set comes first, so that a crash leaves the node with
        // two names rather than none.
        let slot = self.insert(to, &set)?;
        self.release_set(&node)?;
        Ok(self.node(to, slot, set))
    }

    /// Whether `dir` is `ancestor` or lies below it.
    fn is_within(&self, dir: Dir, ancestor: Dir) -> Result<bool, Error> {
        let mut current = dir;
        for _ in 0..=self.dirs.len() {
            if current == ancestor {
                return Ok(true);
            }
            current = match current {
                Dir::Root => return Ok(false),
                Dir::Sub(first) => self.dirs.get(&first).ok_or(Error::NotFound)?.parent,
            };
        }
        Err(Error::Io)
    }

    /// Writes `set` into free entries in a row in `dir`, growing it a
    /// cluster at a time until it has them, and gives the slot of its
    /// file entry.
    pub(crate) fn insert(&mut self, dir: Dir, set: &Set) -> Result<u32, Error> {
        let encoded = set.encode(&self.upcase);
        let count = encoded.len() / ENTRY;
        let slot = loop {
            if let Some(slot) = free_slots(&self.dir_bytes(dir)?, count) {
                break slot;
            }
            self.grow_dir(dir)?;
        };
        // The file entry goes last: until it is written, its checksum does
        // not hold.
        for (i, e) in encoded.chunks(ENTRY).enumerate().rev() {
            let at = self.entry_at(dir, slot + i as u32)?;
            self.disk.write(at, e)?;
        }
        Ok(slot)
    }

    /// Grows `dir` by a zeroed cluster: the root along its chain, and any
    /// other directory as a file grows, with its set's size after that.
    fn grow_dir(&mut self, dir: Dir) -> Result<(), Error> {
        let cluster = u64::from(self.layout.cluster_size);
        let (first, contiguous, size) = self.dir_extent(dir)?;
        let count = match size {
            Some(size) => size.div_ceil(cluster),
            None => self.clusters(first, false, None)?.len() as u64,
        };
        if (count + 1) * cluster > DIR_MAX {
            return Err(Error::NoSpace);
        }
        let mut extent = Extent {
            first,
            contiguous,
            count,
        };
        self.grow(&mut extent, count + 1, true)?;
        let Dir::Sub(first) = dir else {
            return Ok(());
        };
        let place = *self.dirs.get(&first).ok_or(Error::NotFound)?;
        let set = self.set_at(place.parent, place.slot)?;
        let mut node = self.node(place.parent, place.slot, set);
        extent.apply(&mut node.set);
        node.set.size = (count + 1) * cluster;
        node.set.valid_size = node.set.size;
        self.store(&node)?;
        self.dirs.insert(
            first,
            Place {
                contiguous: extent.contiguous,
                size: node.set.size,
                ..place
            },
        );
        Ok(())
    }

    /// Marks the entries of `node`'s set as no longer in use, its file
    /// entry first.
    fn release_set(&mut self, node: &Node) -> Result<(), Error> {
        let mut file = [0u8; ENTRY];
        let first = self.entry_at(node.parent, node.slot)?;
        self.disk.read(first, &mut file)?;
        let count = usize::from(file[1]) + 1;
        for i in 0..count {
            let at = self.entry_at(node.parent, node.slot + i as u32)?;
            let mut e = [0u8; ENTRY];
            self.disk.read(at, &mut e)?;
            e[0] &= !IN_USE;
            self.disk.write(at, &e)?;
        }
        Ok(())
    }
}

/// The UTF-16 code units of `name`, when a new entry may take it.
fn check_name(name: &str) -> Result<Vec<u16>, Error> {
    if !mikros_fat::valid_long(name) {
        return Err(Error::Invalid);
    }
    let units: Vec<u16> = name.encode_utf16().collect();
    if units.len() > NAME_MAX {
        return Err(Error::NameTooLong);
    }
    Ok(units)
}

/// The first slot of `count` free entries in a row among the entries
/// `bytes` of a directory: entries not in use, and all from its end entry
/// on.
fn free_slots(bytes: &[u8], count: usize) -> Option<u32> {
    let mut run = 0;
    let mut ended = false;
    for (slot, e) in bytes.as_chunks::<ENTRY>().0.iter().enumerate() {
        ended |= e[0] == entry::END;
        run = match ended || e[0] & IN_USE == 0 {
            true => run + 1,
            false => 0,
        };
        if run == count {
            return Some((slot + 1 - count) as u32);
        }
    }
    None
}
