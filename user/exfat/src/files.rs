//! Reading and writing files.

use crate::allocation::Extent;
use crate::dir::Node;
use crate::entry::{ARCHIVE, Stamp};
use crate::volume::{BLOCK, Volume};
use crate::{Device, Error};
use alloc::vec;

impl<D: Device> Volume<D> {
    /// Reads up to `buf.len()` bytes of the file `node` from `offset`,
    /// and returns how many it read, 0 at the end. Bytes past its valid
    /// data length read as zeros.
    pub fn read(&mut self, node: &mut Node, offset: u64, buf: &mut [u8]) -> Result<usize, Error> {
        if node.is_directory() {
            return Err(Error::IsDirectory);
        }
        let size = node.set.size;
        if offset >= size || buf.is_empty() {
            return Ok(0);
        }
        let len = (size - offset).min(buf.len() as u64) as usize;
        let valid = node.set.valid_size.min(size);
        let stored = (valid.saturating_sub(offset) as usize).min(len);
        let mut done = 0;
        while done < stored {
            let (at, n) = self.extent(node, offset + done as u64, stored - done)?;
            self.read_bytes(at, &mut buf[done..done + n])?;
            done += n;
        }
        buf[stored..len].fill(0);
        Ok(len)
    }

    /// Where byte `offset` of the file `node` lies on the device, and how
    /// many of the `len` bytes from it lie in a row there.
    pub(crate) fn extent(
        &mut self,
        node: &mut Node,
        offset: u64,
        len: usize,
    ) -> Result<(u64, usize), Error> {
        let cluster = u64::from(self.layout.cluster_size);
        let (first, contiguous) = (node.set.first_cluster, node.set.contiguous());
        let index = offset / cluster;
        let within = offset % cluster;
        let c = self.cluster_at(first, contiguous, index, &mut node.near)?;
        let n = match contiguous {
            true => {
                let last = (offset + len as u64 - 1) / cluster;
                self.cluster_at(first, true, last, &mut (0, 0))?;
                len
            }
            false => ((cluster - within) as usize).min(len),
        };
        Ok((self.layout.cluster_start(c) + within, n))
    }

    /// Reads `buf.len()` bytes at `at`, whole blocks straight from the
    /// device.
    fn read_bytes(&mut self, at: u64, buf: &mut [u8]) -> Result<(), Error> {
        let start = at - at % BLOCK;
        let end = (at + buf.len() as u64).next_multiple_of(BLOCK);
        if start == at && end == at + buf.len() as u64 {
            return self.disk.read_direct(at, buf);
        }
        let mut whole = vec![0u8; (end - start) as usize];
        self.disk.read_direct(start, &mut whole)?;
        let from = (at - start) as usize;
        buf.copy_from_slice(&whole[from..from + buf.len()]);
        Ok(())
    }

    /// Writes `data` at `at`, whole blocks straight to the device.
    fn write_bytes(&mut self, at: u64, data: &[u8]) -> Result<(), Error> {
        let start = at - at % BLOCK;
        let end = (at + data.len() as u64).next_multiple_of(BLOCK);
        if start == at && end == at + data.len() as u64 {
            return self.disk.write_direct(at, data);
        }
        let mut whole = vec![0u8; (end - start) as usize];
        self.disk.read_direct(start, &mut whole)?;
        let from = (at - start) as usize;
        whole[from..from + data.len()].copy_from_slice(data);
        self.disk.write_direct(start, &whole)
    }

    /// Writes zeros over the bytes of the file `node` from `from` to `to`,
    /// which its clusters hold.
    fn zero_range(&mut self, node: &mut Node, from: u64, to: u64) -> Result<(), Error> {
        let zeros = vec![0u8; self.layout.cluster_size as usize];
        let mut at = from;
        while at < to {
            let len = ((to - at) as usize).min(zeros.len());
            let (place, n) = self.extent(node, at, len)?;
            self.write_bytes(place, &zeros[..n])?;
            at += n as u64;
        }
        Ok(())
    }

    /// Gives the file `node` `size` bytes, and the clusters they take, and
    /// writes its set. The clusters it no longer holds are freed after
    /// that.
    fn resize(&mut self, node: &mut Node, size: u64) -> Result<(), Error> {
        let cluster = self.layout.cluster_size;
        let mut extent = Extent::of(&node.set, cluster);
        let count = size.div_ceil(u64::from(cluster));
        self.grow(&mut extent, count, false)?;
        let freed = self.shrink(&mut extent, count)?;
        extent.apply(&mut node.set);
        node.set.size = size;
        node.set.valid_size = node.set.valid_size.min(size);
        node.near = (node.set.first_cluster, 0);
        self.store(node)?;
        self.release(&freed)
    }

    /// Writes `data` to the file `node` at `offset`, growing it as
    /// needed, with the bytes between its valid data and `offset` zero,
    /// and marks it modified at `now`.
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
        if end > node.set.size {
            self.resize(node, end)?;
        }
        let valid = node.set.valid_size;
        if offset > valid {
            self.zero_range(node, valid, offset)?;
        }
        let mut done = 0;
        while done < data.len() {
            let (at, n) = self.extent(node, offset + done as u64, data.len() - done)?;
            self.write_bytes(at, &data[done..done + n])?;
            done += n;
        }
        node.set.valid_size = valid.max(end);
        node.set.modified = now;
        node.set.attributes |= ARCHIVE;
        self.store(node)
    }

    /// Sets the size of the file `node` to `size`, freeing the clusters
    /// past it or taking the ones it needs, whose bytes read as zeros as
    /// they lie past its valid data, and marks it modified at `now`.
    pub fn truncate(&mut self, node: &mut Node, size: u64, now: Stamp) -> Result<(), Error> {
        self.check_writable()?;
        if node.is_directory() {
            return Err(Error::IsDirectory);
        }
        node.set.modified = now;
        node.set.attributes |= ARCHIVE;
        self.resize(node, size)
    }

    /// Sets the access and modification times of `node`, when given.
    pub fn set_times(
        &mut self,
        node: &mut Node,
        accessed: Option<Stamp>,
        modified: Option<Stamp>,
    ) -> Result<(), Error> {
        self.check_writable()?;
        if let Some(accessed) = accessed {
            node.set.accessed = accessed;
        }
        if let Some(modified) = modified {
            node.set.modified = modified;
        }
        self.store(node)
    }

    /// How many bytes the clusters of `node` take.
    pub fn allocated(&mut self, node: &Node) -> Result<u64, Error> {
        let cluster = u64::from(self.layout.cluster_size);
        let set = &node.set;
        let count = match set.contiguous() || set.first_cluster == 0 {
            true => set.size.div_ceil(cluster),
            false => self.clusters(set.first_cluster, false, None)?.len() as u64,
        };
        Ok(count * cluster)
    }
}
