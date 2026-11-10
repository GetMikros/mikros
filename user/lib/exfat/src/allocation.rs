//! Allocating clusters: in a row without a chain while the clusters after
//! an allocation are free, and in a chain in the FAT otherwise, with each
//! change marked in the allocation bitmap.

use crate::entry::{ALLOCATION_POSSIBLE, NO_FAT_CHAIN, Set};
use crate::volume::{BLOCK, END_OF_CHAIN, Volume};
use crate::{Device, Error};
use alloc::collections::BTreeSet;
use alloc::vec;
use alloc::vec::Vec;

/// The clusters of an allocation: its first cluster, whether they lie in
/// a row without a chain, and how many there are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Extent {
    pub(crate) first: u32,
    pub(crate) contiguous: bool,
    pub(crate) count: u64,
}

impl Extent {
    /// The allocation of `set`: as many clusters of `cluster` bytes as
    /// its size takes.
    pub(crate) fn of(set: &Set, cluster: u32) -> Extent {
        Extent {
            first: set.first_cluster,
            contiguous: set.contiguous(),
            count: set.size.div_ceil(u64::from(cluster)),
        }
    }

    /// Gives `set` this allocation. One without clusters has no first
    /// cluster and no flag for a row, as fsck.exfat wants.
    pub(crate) fn apply(&self, set: &mut Set) {
        let row = self.contiguous && self.count > 0;
        set.first_cluster = if self.count > 0 { self.first } else { 0 };
        set.flags = ALLOCATION_POSSIBLE | if row { NO_FAT_CHAIN } else { 0 };
    }
}

impl<D: Device> Volume<D> {
    pub(crate) fn check_writable(&self) -> Result<(), Error> {
        match self.writable {
            true => Ok(()),
            false => Err(Error::ReadOnly),
        }
    }

    /// Chains `clusters` in the FAT in their order, the last one ending
    /// the chain, writing the entries of clusters in a row at once.
    fn link(&mut self, clusters: &[u32]) -> Result<(), Error> {
        let mut i = 0;
        while i < clusters.len() {
            let mut j = i + 1;
            while j < clusters.len() && clusters[j] == clusters[j - 1] + 1 {
                j += 1;
            }
            let mut entries = Vec::with_capacity((j - i) * 4);
            for k in i..j {
                let next = clusters.get(k + 1).copied().unwrap_or(END_OF_CHAIN);
                entries.extend_from_slice(&next.to_le_bytes());
            }
            let at = self.layout.fat_start + u64::from(clusters[i]) * 4;
            self.disk.write(at, &entries)?;
            i = j;
        }
        Ok(())
    }

    /// Sets the FAT entry of `cluster` to `value`.
    fn set_fat(&mut self, cluster: u32, value: u32) -> Result<(), Error> {
        let at = self.layout.fat_start + u64::from(cluster) * 4;
        self.disk.write(at, &value.to_le_bytes())
    }

    /// Marks `clusters` as in use or free, in the bitmap and on the
    /// volume, writing each block of the bitmap that changes once.
    fn mark(&mut self, clusters: &[u32], used: bool) -> Result<(), Error> {
        let block = BLOCK as usize;
        let blocks: BTreeSet<usize> = clusters
            .iter()
            .map(|&c| self.bitmap.set(c, used) / block)
            .collect();
        let size = self.layout.cluster_size as usize;
        for b in blocks {
            let start = b * block;
            let cluster = *self.bitmap.clusters.get(start / size).ok_or(Error::Io)?;
            let at = self.layout.cluster_start(cluster) + (start % size) as u64;
            let bytes = &self.bitmap.bytes[start..start + block];
            self.disk.write_direct(at, bytes)?;
        }
        Ok(())
    }

    /// Takes the free clusters `clusters`, writing zeros over them first
    /// when `zero`.
    fn take(&mut self, clusters: &[u32], zero: bool) -> Result<(), Error> {
        if zero {
            let zeros = vec![0u8; self.layout.cluster_size as usize];
            for &c in clusters {
                self.disk
                    .write_direct(self.layout.cluster_start(c), &zeros)?;
            }
        }
        if let Some(&last) = clusters.last() {
            self.hint = last + 1;
        }
        self.mark(clusters, true)
    }

    /// Makes `extent` hold `count` clusters: the ones after it when it
    /// lies in a row and they are free, and otherwise more in a chain,
    /// after its row, if it had one, became a chain. New clusters are
    /// zeroed when `zero`. Fails with `NoSpace`, taking nothing, when too
    /// few clusters are free.
    pub(crate) fn grow(
        &mut self,
        extent: &mut Extent,
        count: u64,
        zero: bool,
    ) -> Result<(), Error> {
        let Some(more) = count.checked_sub(extent.count).filter(|&m| m > 0) else {
            return Ok(());
        };
        if more > u64::from(self.bitmap.free) {
            return Err(Error::NoSpace);
        }
        let more = more as u32;
        if extent.count == 0 {
            if let Some(first) = self.bitmap.run(self.hint, more) {
                let row: Vec<u32> = (first..first + more).collect();
                self.take(&row, zero)?;
                *extent = Extent {
                    first,
                    contiguous: true,
                    count,
                };
                return Ok(());
            }
            let chain = self.bitmap.take(self.hint, more).ok_or(Error::NoSpace)?;
            self.take(&chain, zero)?;
            self.link(&chain)?;
            *extent = Extent {
                first: chain[0],
                contiguous: false,
                count,
            };
            return Ok(());
        }
        if extent.contiguous {
            let end = u64::from(extent.first) + extent.count;
            let after = (end..end + u64::from(more))
                .map(|c| u32::try_from(c).ok().filter(|&c| self.bitmap.is_free(c)))
                .collect::<Option<Vec<u32>>>();
            if let Some(after) = after {
                self.take(&after, zero)?;
                extent.count = count;
                return Ok(());
            }
            let row: Vec<u32> = (extent.first..end as u32).collect();
            self.link(&row)?;
            extent.contiguous = false;
        }
        let chain = self.clusters(extent.first, false, Some(extent.count))?;
        let last = *chain.last().ok_or(Error::Io)?;
        let new = self.bitmap.take(last + 1, more).ok_or(Error::NoSpace)?;
        self.take(&new, zero)?;
        self.link(&new)?;
        self.set_fat(last, new[0])?;
        extent.count = count;
        Ok(())
    }

    /// Makes `extent` hold only its first `count` clusters, ending its
    /// chain at the last one kept, and gives the others, for `release`
    /// once nothing refers to them.
    pub(crate) fn shrink(&mut self, extent: &mut Extent, count: u64) -> Result<Vec<u32>, Error> {
        if count >= extent.count {
            return Ok(Vec::new());
        }
        let clusters = self.clusters(extent.first, extent.contiguous, Some(extent.count))?;
        let (kept, freed) = clusters.split_at(count as usize);
        if let Some(&last) = kept.last()
            && !extent.contiguous
        {
            self.set_fat(last, END_OF_CHAIN)?;
        }
        extent.count = count;
        Ok(freed.to_vec())
    }

    /// Marks `clusters` as free.
    pub(crate) fn release(&mut self, clusters: &[u32]) -> Result<(), Error> {
        self.mark(clusters, false)
    }

    /// The clusters of `set`.
    pub(crate) fn clusters_of(&mut self, set: &Set) -> Result<Vec<u32>, Error> {
        let extent = Extent::of(set, self.layout.cluster_size);
        self.clusters(extent.first, extent.contiguous, Some(extent.count))
    }
}
