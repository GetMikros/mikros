//! A mounted volume: its boot region and dirty flag, the up-case table
//! and allocation bitmap that the root directory gives, and the clusters
//! of an allocation, in a row or in a chain in the FAT.

use crate::boot::{self, Layout};
use crate::dir::Place;
use crate::entry::{self, ENTRY};
use crate::upcase::{self, Upcase};
use crate::{Device, Error};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use mikros_fat::Disk;

/// The value of a FAT entry that ends a chain.
pub(crate) const END_OF_CHAIN: u32 = 0xFFFF_FFFF;
/// The unit of the device's I/O, and of the cache.
pub(crate) const BLOCK: u64 = 512;

/// A mounted volume on `D`.
pub struct Volume<D> {
    pub(crate) disk: Disk<D>,
    pub(crate) layout: Layout,
    pub(crate) upcase: Upcase,
    /// How many clusters the allocation bitmap marks free.
    pub(crate) free: u32,
    pub(crate) writable: bool,
    /// Whether the volume is marked dirty only while a change is in
    /// progress, as `begin_change` and `end_change` mark it, rather than
    /// from mounting to `unmount`.
    pub(crate) quick: bool,
    /// Where each directory found so far lies, by its first cluster.
    pub(crate) dirs: BTreeMap<u32, Place>,
}

impl<D: Device> Volume<D> {
    /// Mounts the volume on `device`, a partition of `size` bytes, for
    /// writing when `writable`. A volume marked dirty already, or whose
    /// main boot region is invalid, is mounted read-only. Mounting for
    /// writing marks the volume dirty until `unmount`.
    pub fn mount(device: D, size: u64, writable: bool) -> Result<Volume<D>, Error> {
        Self::open(device, size, writable, false)
    }

    /// Mounts the volume as `mount` does, but leaves it marked clean:
    /// `begin_change` and `end_change` mark it dirty while each change is
    /// in progress.
    pub fn mount_quick_disconnect(
        device: D,
        size: u64,
        writable: bool,
    ) -> Result<Volume<D>, Error> {
        Self::open(device, size, writable, true)
    }

    fn open(device: D, size: u64, writable: bool, quick: bool) -> Result<Volume<D>, Error> {
        let mut disk = Disk::new(device, BLOCK as u32);
        let layout = boot_region(&mut disk, size)?;
        let clean = layout.flags & boot::VOLUME_DIRTY == 0;
        let mut volume = Volume {
            disk,
            writable: writable && clean && layout.region == 0,
            layout,
            upcase: Upcase::default(),
            free: 0,
            quick,
            dirs: BTreeMap::new(),
        };
        volume.read_critical()?;
        if volume.writable && !quick {
            volume.set_dirty(true)?;
            volume.disk.device.flush()?;
        }
        Ok(volume)
    }

    /// Reads the up-case table, whose checksum must hold, and the
    /// allocation bitmap of the FAT in use, from their entries in the
    /// root directory.
    fn read_critical(&mut self) -> Result<(), Error> {
        let root = self.root_bytes()?;
        let (mut bitmap, mut table) = (None, None);
        for e in root.as_chunks::<ENTRY>().0 {
            match e[0] {
                entry::END => break,
                // Bit 0 of the flags gives the FAT the bitmap belongs to.
                entry::BITMAP if bitmap.is_none() && e[1] & 1 == self.layout.active => {
                    bitmap = Some(entry::extent(e))
                }
                entry::UPCASE if table.is_none() => {
                    table = Some((entry::extent(e), entry::table_checksum(e)))
                }
                _ => {}
            }
        }
        let cluster = u64::from(self.layout.cluster_size);
        let ((first, len), checksum) = table.ok_or(Error::Invalid)?;
        let clusters = self.clusters(first, false, Some(len.div_ceil(cluster)))?;
        let mut bytes = self.read_clusters(&clusters, false)?;
        bytes.truncate(len as usize);
        if upcase::checksum(&bytes) != checksum {
            return Err(Error::Invalid);
        }
        self.upcase = Upcase::parse(&bytes);
        let (first, len) = bitmap.ok_or(Error::Invalid)?;
        if len.saturating_mul(8) < u64::from(self.layout.clusters) {
            return Err(Error::Invalid);
        }
        let clusters = self.clusters(first, false, Some(len.div_ceil(cluster)))?;
        let bytes = self.read_clusters(&clusters, false)?;
        let used: u32 = (0..self.layout.clusters)
            .map(|i| u32::from(bytes[(i / 8) as usize] >> (i % 8) & 1))
            .sum();
        self.free = self.layout.clusters - used;
        Ok(())
    }

    /// The bytes of the clusters `clusters`, read through the cache when
    /// `cached`, and otherwise straight from the device.
    pub(crate) fn read_clusters(
        &mut self,
        clusters: &[u32],
        cached: bool,
    ) -> Result<Vec<u8>, Error> {
        let size = self.layout.cluster_size as usize;
        let mut bytes = vec![0u8; clusters.len() * size];
        for (chunk, &c) in bytes.chunks_mut(size).zip(clusters) {
            let at = self.layout.cluster_start(c);
            match cached {
                true => self.disk.read(at, chunk)?,
                false => self.disk.read_direct(at, chunk)?,
            }
        }
        Ok(bytes)
    }

    /// The entries of the root directory, read through the cache.
    pub(crate) fn root_bytes(&mut self) -> Result<Vec<u8>, Error> {
        let clusters = self.clusters(self.layout.root_cluster, false, None)?;
        self.read_clusters(&clusters, true)
    }

    /// Sets or clears the dirty flag in the main boot sector, with the
    /// share of the heap in use.
    pub(crate) fn set_dirty(&mut self, dirty: bool) -> Result<(), Error> {
        let flags = match dirty {
            true => self.layout.flags | boot::VOLUME_DIRTY,
            false => self.layout.flags & !boot::VOLUME_DIRTY,
        };
        let clusters = u64::from(self.layout.clusters);
        let used = clusters - u64::from(self.free);
        let mut fields = [0u8; 7];
        self.disk.read(boot::FLAGS_AT as u64, &mut fields)?;
        fields[0..2].copy_from_slice(&flags.to_le_bytes());
        fields[6] = (used * 100 / clusters) as u8;
        self.disk.write(boot::FLAGS_AT as u64, &fields)?;
        self.layout.flags = flags;
        Ok(())
    }

    /// Marks a volume mounted with `mount_quick_disconnect` for writing
    /// as dirty, on stable storage, before a change.
    pub fn begin_change(&mut self) -> Result<(), Error> {
        if !self.writable || !self.quick {
            return Ok(());
        }
        self.set_dirty(true)?;
        self.disk.device.flush()
    }

    /// Flushes the device after a change, and marks a volume mounted with
    /// `mount_quick_disconnect` for writing as clean again, on stable
    /// storage.
    pub fn end_change(&mut self) -> Result<(), Error> {
        self.disk.device.flush()?;
        if !self.writable || !self.quick {
            return Ok(());
        }
        self.set_dirty(false)?;
        self.disk.device.flush()
    }

    /// Marks the volume clean and flushes the device, when it is mounted
    /// for writing. It stays mounted read-only.
    pub fn unmount(&mut self) -> Result<(), Error> {
        if !self.writable {
            return Ok(());
        }
        self.disk.device.flush()?;
        self.set_dirty(false)?;
        self.disk.device.flush()?;
        self.writable = false;
        Ok(())
    }

    /// Makes everything written durable.
    pub fn sync(&mut self) -> Result<(), Error> {
        self.disk.device.flush()
    }

    pub fn device(&mut self) -> &mut D {
        &mut self.disk.device
    }

    pub fn into_device(self) -> D {
        self.disk.device
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn read_only(&self) -> bool {
        !self.writable
    }

    /// How many clusters are free.
    pub fn free_clusters(&self) -> u32 {
        self.free
    }

    /// The FAT entry of `cluster`.
    pub(crate) fn fat(&mut self, cluster: u32) -> Result<u32, Error> {
        let mut entry = [0u8; 4];
        self.disk
            .read(self.layout.fat_start + u64::from(cluster) * 4, &mut entry)?;
        Ok(u32::from_le_bytes(entry))
    }

    /// The clusters of an allocation that starts at `first`: `count` of
    /// them in a row when `contiguous`, and otherwise its chain in the
    /// FAT, which must hold `count` of them, or to its end when `count` is
    /// `None`. Fails with `Io` for clusters outside the heap and for a
    /// chain longer than the heap.
    pub(crate) fn clusters(
        &mut self,
        first: u32,
        contiguous: bool,
        count: Option<u64>,
    ) -> Result<Vec<u32>, Error> {
        let heap = u64::from(self.layout.clusters);
        if count == Some(0) {
            return Ok(Vec::new());
        }
        if contiguous {
            let count = count.ok_or(Error::Io)?;
            if !self.layout.is_data(first) || u64::from(first - 2) + count > heap {
                return Err(Error::Io);
            }
            return Ok((first..first + count as u32).collect());
        }
        let mut chain = Vec::new();
        let mut at = first;
        loop {
            if !self.layout.is_data(at) || chain.len() as u64 >= heap {
                return Err(Error::Io);
            }
            chain.push(at);
            if count == Some(chain.len() as u64) {
                return Ok(chain);
            }
            match self.fat(at)? {
                END_OF_CHAIN if count.is_none() => return Ok(chain),
                next => at = next,
            }
        }
    }

    /// Cluster `index` of an allocation that starts at `first`, walking a
    /// chain from `near` when it lies before, and keeping the cluster
    /// found there.
    pub(crate) fn cluster_at(
        &mut self,
        first: u32,
        contiguous: bool,
        index: u64,
        near: &mut (u32, u64),
    ) -> Result<u32, Error> {
        let heap = u64::from(self.layout.clusters);
        if index >= heap || !self.layout.is_data(first) {
            return Err(Error::Io);
        }
        if contiguous {
            let at = u64::from(first) + index;
            return match at < heap + 2 {
                true => Ok(at as u32),
                false => Err(Error::Io),
            };
        }
        let (mut at, mut i) = match near.0 != 0 && near.1 <= index {
            true => *near,
            false => (first, 0),
        };
        while i < index {
            at = self.fat(at)?;
            if !self.layout.is_data(at) {
                return Err(Error::Io);
            }
            i += 1;
        }
        *near = (at, i);
        Ok(at)
    }

    /// The volume label, or `None` when the volume has none.
    pub fn label(&mut self) -> Result<Option<String>, Error> {
        let root = self.root_bytes()?;
        for e in root.as_chunks::<ENTRY>().0 {
            match e[0] {
                entry::END => break,
                entry::LABEL => {
                    let label = entry::label(e);
                    return Ok((!label.is_empty()).then_some(label));
                }
                _ => {}
            }
        }
        Ok(None)
    }
}

/// The layout from the main boot region, or, when that is invalid, from
/// the backup region, which may be at any of the sector sizes.
fn boot_region<D: Device>(disk: &mut Disk<D>, size: u64) -> Result<Layout, Error> {
    let largest = boot::REGION_SECTORS << 12;
    if size >= largest {
        let mut region = vec![0u8; largest as usize];
        disk.read_direct(0, &mut region)?;
        if let Ok(layout) = Layout::parse(&region, 0, size) {
            return Ok(layout);
        }
    }
    for shift in 9..=12 {
        let len = boot::REGION_SECTORS << shift;
        if 2 * len > size {
            continue;
        }
        let mut region = vec![0u8; len as usize];
        disk.read_direct(len, &mut region)?;
        if let Ok(layout) = Layout::parse(&region, boot::REGION_SECTORS, size)
            && layout.sector_size == 1 << shift
        {
            return Ok(layout);
        }
    }
    Err(Error::Invalid)
}
