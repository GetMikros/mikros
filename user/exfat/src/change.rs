//! Creating the entries of directories, and growing directories.

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
