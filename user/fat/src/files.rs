//! Writing files: allocating and freeing clusters, and changing what
//! files hold and when they were changed.

use crate::dir::{ARCHIVE, Stamp};
use crate::table::{self, Entry};
use crate::volume::{FILE_MAX, Node, Volume};
use crate::{Device, Error};
use alloc::vec;

impl<D: Device> Volume<D> {
    pub(crate) fn check_writable(&self) -> Result<(), Error> {
        match self.writable {
            true => Ok(()),
            false => Err(Error::ReadOnly),
        }
    }

    /// How many clusters the chain that starts at `first` has, and its
    /// last.
    fn chain_end(&mut self, first: u32) -> Result<(u32, u32), Error> {
        let (mut count, mut last) = (1, first);
        while let Some(next) = table::next(&mut self.disk, &self.layout, last)? {
            count += 1;
            last = next;
            if count > self.layout.clusters {
                return Err(Error::Io);
            }
        }
        Ok((count, last))
    }

    /// A free cluster, marked as the end of a chain, linked after
    /// `after` when there is one.
    pub(crate) fn allocate(&mut self, after: Option<u32>) -> Result<u32, Error> {
        let total = self.layout.clusters;
        let start = if self.layout.is_data(self.hint) {
            self.hint
        } else {
            2
        };
        for k in 0..total {
            let cluster = 2 + (start - 2 + k) % total;
            if table::get(&mut self.disk, &self.layout, cluster)? == Entry::Free {
                table::set(&mut self.disk, &self.layout, cluster, Entry::End)?;
                if let Some(after) = after {
                    table::set(&mut self.disk, &self.layout, after, Entry::Next(cluster))?;
                }
                self.hint = cluster + 1;
                if let Some(free) = &mut self.free {
                    *free -= 1;
                }
                return Ok(cluster);
            }
        }
        Err(Error::NoSpace)
    }

    /// Frees the chain that starts at `first`.
    pub(crate) fn free_chain(&mut self, first: u32) -> Result<(), Error> {
        let mut cluster = Some(first);
        let mut count = 0;
        while let Some(c) = cluster {
            cluster = table::next(&mut self.disk, &self.layout, c)?;
            table::set(&mut self.disk, &self.layout, c, Entry::Free)?;
            if let Some(free) = &mut self.free {
                *free += 1;
            }
            count += 1;
            if count > self.layout.clusters {
                return Err(Error::Io);
            }
        }
        Ok(())
    }

    pub(crate) fn write_entry(&mut self, node: &Node) -> Result<(), Error> {
        let at = self.slot_at(node.parent, node.slot)?.ok_or(Error::Io)?;
        self.disk.write(at, &node.entry.encode())
    }

    /// Writes `data` at `at`, whole sectors straight to the device.
    fn write_bytes(&mut self, at: u64, data: &[u8]) -> Result<(), Error> {
        let sector = self.disk.sector();
        let start = at - at % sector;
        let end = (at + data.len() as u64).next_multiple_of(sector);
        if start == at && end == at + data.len() as u64 {
            return self.disk.write_direct(at, data);
        }
        let mut whole = vec![0u8; (end - start) as usize];
        self.disk.read_direct(start, &mut whole)?;
        let from = (at - start) as usize;
        whole[from..from + data.len()].copy_from_slice(data);
        self.disk.write_direct(start, &whole)
    }

    /// Makes the file `node` hold `count` clusters at least.
    fn grow(&mut self, node: &mut Node, count: u32) -> Result<(), Error> {
        if count == 0 {
            return Ok(());
        }
        let (mut have, mut last) = match node.entry.cluster {
            0 => (0, None),
            first => {
                let (have, last) = self.chain_end(first)?;
                (have, Some(last))
            }
        };
        while have < count {
            let cluster = self.allocate(last)?;
            if last.is_none() {
                node.entry.cluster = cluster;
                node.near = (cluster, 0);
            }
            last = Some(cluster);
            have += 1;
        }
        Ok(())
    }

    /// Writes zeros over the bytes of the file `node` from `from` to
    /// `to`, which its clusters hold.
    fn zero_range(&mut self, node: &mut Node, from: u64, to: u64) -> Result<(), Error> {
        let zeros = vec![0u8; self.layout.cluster_size as usize];
        let mut at = from;
        while at < to {
            let (place, n) = self.extent(node, at, (to - at) as usize)?;
            self.write_bytes(place, &zeros[..n])?;
            at += n as u64;
        }
        Ok(())
    }

    /// Writes `data` to the file `node` at `offset`, growing it as
    /// needed, with the bytes between its end and `offset` zero, and
    /// marks it modified at `now`.
    pub fn write(
        &mut self,
        node: &mut Node,
        offset: u64,
        data: &[u8],
        now: Stamp,
    ) -> Result<(), Error> {
        self.check_writable()?;
        if node.is_directory() {
            return Err(Error::IsDirectory);
        }
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(Error::NoSpace)?;
        if end > FILE_MAX {
            return Err(Error::NoSpace);
        }
        let size = node.size();
        let clusters = end.max(size).div_ceil(u64::from(self.layout.cluster_size)) as u32;
        let grown = self.grow(node, clusters);
        // Clusters taken before running out stay with the file, which
        // keeps its size, so that the entry and the chain agree.
        self.write_entry(node)?;
        grown?;
        if offset > size {
            self.zero_range(node, size, offset)?;
        }
        let mut done = 0;
        while done < data.len() {
            let (at, n) = self.extent(node, offset + done as u64, data.len() - done)?;
            self.write_bytes(at, &data[done..done + n])?;
            done += n;
        }
        node.entry.size = end.max(size) as u32;
        node.entry.modified = Stamp {
            hundredths: 0,
            ..now
        };
        node.entry.attributes |= ARCHIVE;
        self.write_entry(node)
    }

    /// Sets the size of the file `node` to `size`, freeing the clusters
    /// past it or zeroing the bytes it gains, and marks it modified at
    /// `now`.
    pub fn truncate(&mut self, node: &mut Node, size: u64, now: Stamp) -> Result<(), Error> {
        self.check_writable()?;
        if node.is_directory() {
            return Err(Error::IsDirectory);
        }
        if size > FILE_MAX {
            return Err(Error::NoSpace);
        }
        let old = node.size();
        let clusters = size.div_ceil(u64::from(self.layout.cluster_size)) as u32;
        if size > old {
            let grown = self.grow(node, clusters);
            self.write_entry(node)?;
            grown?;
            self.zero_range(node, old, size)?;
        } else if node.entry.cluster != 0 {
            if clusters == 0 {
                let first = node.entry.cluster;
                node.entry.cluster = 0;
                node.entry.size = 0;
                self.write_entry(node)?;
                self.free_chain(first)?;
            } else {
                let last = self.cluster_at(node.entry.cluster, clusters - 1, (0, 0))?;
                if let Some(rest) = table::next(&mut self.disk, &self.layout, last)? {
                    table::set(&mut self.disk, &self.layout, last, Entry::End)?;
                    self.free_chain(rest)?;
                }
            }
            node.near = (node.entry.cluster, 0);
        }
        node.entry.size = size as u32;
        node.entry.modified = Stamp {
            hundredths: 0,
            ..now
        };
        node.entry.attributes |= ARCHIVE;
        self.write_entry(node)
    }

    /// Sets the access date and the modification time of `node`, when
    /// given.
    pub fn set_times(
        &mut self,
        node: &mut Node,
        accessed: Option<Stamp>,
        modified: Option<Stamp>,
    ) -> Result<(), Error> {
        self.check_writable()?;
        if let Some(accessed) = accessed {
            node.entry.accessed = accessed.date;
        }
        if let Some(modified) = modified {
            node.entry.modified = Stamp {
                hundredths: 0,
                ..modified
            };
        }
        self.write_entry(node)
    }
}
