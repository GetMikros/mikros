//! Creating, removing and renaming directory entries.

use crate::dir::{self, ARCHIVE, DIRECTORY, ENTRY, FREE, Short, Slot, Stamp};
use crate::names;
use crate::volume::{Dir, Node, Volume};
use crate::{Device, Error};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// The most numeric tails tried for a short name.
const TAILS_MAX: u32 = 999_999;

impl<D: Device> Volume<D> {
    /// Creates the file, or the directory when `directory`, `name` in
    /// `dir`, created and modified at `now`.
    pub fn create(
        &mut self,
        dir: Dir,
        name: &str,
        directory: bool,
        now: Stamp,
    ) -> Result<Node, Error> {
        self.check_writable()?;
        check_name(name)?;
        match self.find(dir, name) {
            Ok(_) => return Err(Error::Exists),
            Err(Error::NotFound) => {}
            Err(e) => return Err(e),
        }
        let mut short = Short {
            attributes: if directory { DIRECTORY } else { ARCHIVE },
            created: now,
            accessed: now.date,
            modified: Stamp {
                hundredths: 0,
                ..now
            },
            ..Short::default()
        };
        if directory {
            let cluster = self.allocate(None)?;
            if let Err(e) = self.init_directory(cluster, dir, &short) {
                let _ = self.free_chain(cluster);
                return Err(e);
            }
            short.cluster = cluster;
        }
        let cluster = short.cluster;
        let inserted = self.insert(dir, name, short);
        if inserted.is_err() && directory {
            // The directory's cluster goes back when its entry could not
            // be written.
            let _ = self.free_chain(cluster);
        }
        inserted
    }

    /// Writes zeros over cluster `cluster`.
    fn zero_cluster(&mut self, cluster: u32) -> Result<(), Error> {
        let zeros = vec![0u8; self.layout.cluster_size as usize];
        self.disk
            .write_direct(self.layout.cluster_start(cluster), &zeros)
    }

    /// Writes the `.` and `..` entries of a new directory in `cluster`,
    /// whose parent is `parent`, zeroing the rest.
    fn init_directory(&mut self, cluster: u32, parent: Dir, short: &Short) -> Result<(), Error> {
        self.zero_cluster(cluster)?;
        let at = self.layout.cluster_start(cluster);
        let mut dot = Short {
            name: *b".          ",
            cluster,
            ..short.clone()
        };
        self.disk.write(at, &dot.encode())?;
        dot.name = *b"..         ";
        dot.cluster = self.dot_dot(parent);
        self.disk.write(at + ENTRY as u64, &dot.encode())
    }

    /// The cluster that `..` names for the parent `parent`: 0 for the
    /// root.
    fn dot_dot(&self, parent: Dir) -> u32 {
        match parent {
            Dir::Cluster(c) if parent != self.root() => c,
            _ => 0,
        }
    }

    /// Writes the entries of `name` with the short entry `short`, with a
    /// unique short name, to free slots of `dir`.
    fn insert(&mut self, dir: Dir, name: &str, mut short: Short) -> Result<Node, Error> {
        short.name = self.unique_short(dir, name)?;
        short.case = 0;
        let mut slots = names::long_entries(name, dir::checksum(&short.name));
        slots.push(short.encode());
        let first = self.free_run(dir, slots.len() as u32)?;
        for (i, slot) in slots.iter().enumerate() {
            let at = self.slot_at(dir, first + i as u32)?.ok_or(Error::Io)?;
            self.disk.write(at, slot)?;
        }
        let slot = first + slots.len() as u32 - 1;
        let near = (short.cluster, 0);
        Ok(Node {
            name: String::from(name),
            parent: dir,
            slot,
            first_slot: first,
            entry: short,
            near,
        })
    }

    /// A short name for `name` that no entry of `dir` has.
    fn unique_short(&mut self, dir: Dir, name: &str) -> Result<[u8; 11], Error> {
        let taken = self.short_names(dir)?;
        let (basis, lossless) = names::short_basis(name);
        if lossless && !taken.contains(&basis) {
            return Ok(basis);
        }
        (1..=TAILS_MAX)
            .map(|n| names::with_tail(&basis, n))
            .find(|candidate| !taken.contains(candidate))
            .ok_or(Error::Exists)
    }

    /// The short names of the entries of `dir`.
    fn short_names(&mut self, dir: Dir) -> Result<Vec<[u8; 11]>, Error> {
        let mut names = Vec::new();
        let mut slot = 0;
        while let Some(at) = self.slot_at(dir, slot)? {
            match Slot::parse(&self.read_slot(at)?) {
                Slot::End => break,
                Slot::Short(short) => names.push(short.name),
                _ => {}
            }
            slot += 1;
        }
        Ok(names)
    }

    /// The first of `count` free slots in a row in `dir`, which grows by
    /// a cluster when it has none, unless it is the fixed root.
    fn free_run(&mut self, dir: Dir, count: u32) -> Result<u32, Error> {
        let (mut start, mut run, mut slot) = (0, 0, 0);
        loop {
            let at = match self.slot_at(dir, slot)? {
                Some(at) => at,
                None => {
                    let Dir::Cluster(first) = dir else {
                        return Err(Error::NoSpace);
                    };
                    let last = self.last_cluster(first)?;
                    let cluster = self.allocate(Some(last))?;
                    self.zero_cluster(cluster)?;
                    continue;
                }
            };
            let free = matches!(Slot::parse(&self.read_slot(at)?), Slot::End | Slot::Free);
            if free {
                if run == 0 {
                    start = slot;
                }
                run += 1;
                if run == count {
                    return Ok(start);
                }
            } else {
                run = 0;
            }
            slot += 1;
        }
    }

    fn last_cluster(&mut self, first: u32) -> Result<u32, Error> {
        let mut cluster = first;
        while let Some(next) = crate::table::next(&mut self.disk, &self.layout, cluster)? {
            cluster = next;
        }
        Ok(cluster)
    }

    /// Marks the slots of `node`'s entries free.
    fn release(&mut self, node: &Node) -> Result<(), Error> {
        for slot in node.first_slot..=node.slot {
            let at = self.slot_at(node.parent, slot)?.ok_or(Error::Io)?;
            self.disk.write(at, &[FREE])?;
        }
        Ok(())
    }

    /// Whether `dir` holds no entries but `.` and `..`.
    fn is_empty(&mut self, dir: Dir) -> Result<bool, Error> {
        Ok(self.list(dir, 0, 1)?.0.is_empty())
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
        if let Some(sub) = node.as_dir(self.root())
            && !self.is_empty(sub)?
        {
            return Err(Error::NotEmpty);
        }
        self.release(&node)?;
        if node.cluster() != 0 {
            self.free_chain(node.cluster())?;
        }
        Ok(())
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
        check_name(new_name)?;
        let node = self.find(from, name)?;
        let moved_dir = node.as_dir(self.root());
        if let Some(moved) = moved_dir
            && self.is_within(to, moved)?
        {
            return Err(Error::Invalid);
        }
        match self.find(to, new_name) {
            Ok(existing) if existing.parent == node.parent && existing.slot == node.slot => {}
            Ok(existing) => {
                match (existing.is_directory(), moved_dir.is_some()) {
                    (true, false) => return Err(Error::IsDirectory),
                    (false, true) => return Err(Error::NotDirectory),
                    _ => {}
                }
                self.remove(to, &existing.name, existing.is_directory())?;
            }
            Err(Error::NotFound) => {}
            Err(e) => return Err(e),
        }
        // The new entry comes first, so that a crash leaves the node with
        // two names rather than none.
        let inserted = self.insert(to, new_name, node.entry.clone())?;
        self.release(&node)?;
        if let Some(moved) = moved_dir
            && from != to
            && let Dir::Cluster(cluster) = moved
        {
            let at = self.dot_dot_at(cluster)?;
            let mut dot_dot = Short::parse(&self.read_slot(at)?);
            dot_dot.cluster = self.dot_dot(to);
            self.disk.write(at, &dot_dot.encode())?;
        }
        Ok(inserted)
    }

    /// Where the `..` entry of the directory at `cluster` lies: among its
    /// first entries, after `.`, and after long-name entries that some
    /// formatters put before both.
    pub(crate) fn dot_dot_at(&mut self, cluster: u32) -> Result<u64, Error> {
        for slot in 0..8 {
            let at = self
                .slot_at(Dir::Cluster(cluster), slot)?
                .ok_or(Error::Io)?;
            match Slot::parse(&self.read_slot(at)?) {
                Slot::Short(short) if short.name == *b"..         " => return Ok(at),
                Slot::End => break,
                _ => {}
            }
        }
        Err(Error::Io)
    }

    /// Whether `dir` is `ancestor` or lies below it.
    fn is_within(&mut self, dir: Dir, ancestor: Dir) -> Result<bool, Error> {
        let mut current = dir;
        for _ in 0..self.layout.clusters {
            if current == ancestor {
                return Ok(true);
            }
            let Dir::Cluster(cluster) = current else {
                return Ok(false);
            };
            if current == self.root() {
                return Ok(false);
            }
            let at = self.dot_dot_at(cluster)?;
            let up = Short::parse(&self.read_slot(at)?);
            current = match up.cluster {
                0 => self.root(),
                c => Dir::Cluster(c),
            };
        }
        Err(Error::Io)
    }
}

/// Checks that `name` can be stored: `Invalid` when FAT cannot hold it,
/// and `NameTooLong` above 255 code units.
fn check_name(name: &str) -> Result<(), Error> {
    if !names::valid_long(name) {
        return Err(Error::Invalid);
    }
    if names::units(name) > dir::NAME_UNITS_MAX {
        return Err(Error::NameTooLong);
    }
    Ok(())
}
