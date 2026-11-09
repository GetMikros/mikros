//! The boot region: the main boot sector's layout of the volume, the
//! checksum over the region, and the backup region that a damaged main
//! one gives way to.

use crate::Error;

/// The sectors of a boot region, the last of which holds the checksum.
pub const REGION_SECTORS: u64 = 12;
/// Where the volume flags lie in the main boot sector.
pub const FLAGS_AT: usize = 106;
const ACTIVE_FAT: u16 = 1 << 0;
const PERCENT_IN_USE_AT: usize = 112;

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// The checksum of the first 11 sectors of a boot region, `region`,
/// which leaves out the volume flags and the percentage in use of the
/// boot sector.
pub fn checksum(region: &[u8], sector: usize) -> u32 {
    let mut sum = 0u32;
    for (i, &byte) in region[..11 * sector].iter().enumerate() {
        if matches!(i, 106 | 107 | PERCENT_IN_USE_AT) {
            continue;
        }
        sum = sum.rotate_right(1).wrapping_add(u32::from(byte));
    }
    sum
}

/// What the main boot sector says about the volume, in bytes where it
/// gives sectors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub sector_size: u32,
    pub cluster_size: u32,
    /// Where the FAT in use starts, and how long a FAT is.
    pub fat_start: u64,
    pub fat_size: u64,
    pub heap_start: u64,
    pub clusters: u32,
    pub root_cluster: u32,
    pub serial: u32,
    pub flags: u16,
    pub total_bytes: u64,
    /// Which FAT and allocation bitmap are in use: 0 for the first, 1
    /// for the second.
    pub active: u8,
    /// The sector of the boot region the layout was read from: 0 for
    /// the main region, 12 for the backup.
    pub region: u64,
}

impl Layout {
    /// The layout of the volume whose boot region, `region`, starts at
    /// sector `at`, on a partition of `partition_bytes`. Fails with
    /// `Invalid` when the region is not a valid exFAT one, its checksum
    /// among it.
    pub fn parse(region: &[u8], at: u64, partition_bytes: u64) -> Result<Layout, Error> {
        if region.len() < 512 || region[0..3] != [0xEB, 0x76, 0x90] || &region[3..11] != b"EXFAT   "
        {
            return Err(Error::Invalid);
        }
        if region[11..64].iter().any(|&b| b != 0) || region[510..512] != [0x55, 0xAA] {
            return Err(Error::Invalid);
        }
        let sector_shift = u32::from(region[108]);
        let cluster_shift = u32::from(region[109]);
        if !(9..=12).contains(&sector_shift) || cluster_shift > 25 - sector_shift {
            return Err(Error::Invalid);
        }
        let sector = 1usize << sector_shift;
        if region.len() < REGION_SECTORS as usize * sector {
            return Err(Error::Invalid);
        }
        let sum = checksum(region, sector);
        let stored = &region[11 * sector..12 * sector];
        if stored
            .as_chunks::<4>()
            .0
            .iter()
            .any(|&c| u32::from_le_bytes(c) != sum)
        {
            return Err(Error::Invalid);
        }
        let fats = u64::from(region[110]);
        let volume_sectors = u64_at(region, 72);
        let fat_offset = u64::from(u32_at(region, 80));
        let fat_sectors = u64::from(u32_at(region, 84));
        let heap_offset = u64::from(u32_at(region, 88));
        let clusters = u32_at(region, 92);
        let root_cluster = u32_at(region, 96);
        let flags = u16_at(region, FLAGS_AT);
        let per_cluster = 1u64 << cluster_shift;
        let total_bytes = volume_sectors
            .checked_mul(sector as u64)
            .ok_or(Error::Invalid)?;
        let fat_bytes = (u64::from(clusters) + 2) * 4;
        let valid = matches!(fats, 1 | 2)
            && fat_offset >= 24
            && fat_sectors * sector as u64 >= fat_bytes
            && heap_offset >= fat_offset + fat_sectors * fats
            && clusters > 0
            && clusters <= 0xFFFF_FFF5
            && heap_offset + u64::from(clusters) * per_cluster <= volume_sectors
            && (2..=clusters + 1).contains(&root_cluster)
            && total_bytes <= partition_bytes;
        if !valid {
            return Err(Error::Invalid);
        }
        // The second FAT is the one in use when the flag says so.
        let active = u64::from(fats == 2 && flags & ACTIVE_FAT != 0);
        Ok(Layout {
            sector_size: sector as u32,
            cluster_size: (sector as u64 * per_cluster) as u32,
            fat_start: (fat_offset + active * fat_sectors) * sector as u64,
            fat_size: fat_sectors * sector as u64,
            heap_start: heap_offset * sector as u64,
            clusters,
            root_cluster,
            serial: u32_at(region, 100),
            flags,
            total_bytes,
            active: active as u8,
            region: at,
        })
    }

    /// The byte offset of cluster `cluster` on the volume.
    pub fn cluster_start(&self, cluster: u32) -> u64 {
        self.heap_start + u64::from(cluster - 2) * u64::from(self.cluster_size)
    }

    /// Whether `cluster` is one of the cluster heap's.
    pub fn is_data(&self, cluster: u32) -> bool {
        cluster >= 2 && cluster - 2 < self.clusters
    }
}
