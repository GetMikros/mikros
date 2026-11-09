//! Reading files.

use crate::dir::Node;
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
