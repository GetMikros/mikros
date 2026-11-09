//! The boot sector: checking its BIOS parameter block, and the layout
//! and FAT type it gives.

use crate::Error;

/// The FAT type, from the cluster count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

/// Where the parts of a volume lie, in bytes from its start, and its
/// sizes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub fat_type: FatType,
    pub sector_size: u32,
    pub cluster_size: u32,
    /// The first FAT, the size of each, how many there are, and the
    /// active one when mirroring is off.
    pub fat_start: u64,
    pub fat_size: u64,
    pub fats: u8,
    pub active_fat: Option<u8>,
    /// The fixed root directory of FAT12 and FAT16, and its entry count.
    pub root_start: u64,
    pub root_entries: u32,
    /// The root directory's first cluster on FAT32.
    pub root_cluster: u32,
    /// The data region, which holds cluster 2 first.
    pub data_start: u64,
    pub clusters: u32,
    /// The FSInfo sector of FAT32, when there is one.
    pub fs_info: Option<u64>,
    pub total_bytes: u64,
    /// The volume label of the extended boot record, when it has one
    /// other than `NO NAME`.
    pub boot_label: Option<[u8; 11]>,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl Layout {
    /// The layout of the volume whose boot sector starts `sector`, which
    /// holds at least 512 bytes, on a partition of `partition_bytes`.
    /// Fails with `Invalid` for a boot sector that is not a valid FAT
    /// one, or a volume larger than the partition.
    pub fn parse(sector: &[u8], partition_bytes: u64) -> Result<Layout, Error> {
        if sector.len() < 512 || sector[510..512] != [0x55, 0xAA] {
            return Err(Error::Invalid);
        }
        if !(sector[0] == 0xEB && sector[2] == 0x90 || sector[0] == 0xE9) {
            return Err(Error::Invalid);
        }
        let sector_size = u32::from(u16_at(sector, 11));
        let per_cluster = u32::from(sector[13]);
        let reserved = u64::from(u16_at(sector, 14));
        let fats = sector[16];
        let root_entries = u32::from(u16_at(sector, 17));
        let total16 = u64::from(u16_at(sector, 19));
        let fat16_size = u64::from(u16_at(sector, 22));
        let total32 = u64::from(u32_at(sector, 32));
        let sizes = [512, 1024, 2048, 4096];
        let cluster_size = sector_size * per_cluster;
        if !sizes.contains(&sector_size)
            || !per_cluster.is_power_of_two()
            || per_cluster > 128
            || cluster_size > 1 << 16
            || reserved == 0
            || fats == 0
        {
            return Err(Error::Invalid);
        }
        let fat32_layout = fat16_size == 0;
        let fat_sectors = match fat32_layout {
            true => u64::from(u32_at(sector, 36)),
            false => fat16_size,
        };
        let total = if total16 != 0 { total16 } else { total32 };
        if fat_sectors == 0 || total == 0 {
            return Err(Error::Invalid);
        }
        let s = u64::from(sector_size);
        let root_sectors = (u64::from(root_entries) * 32).div_ceil(s);
        let data_sector = reserved + u64::from(fats) * fat_sectors + root_sectors;
        if data_sector >= total || total * s > partition_bytes {
            return Err(Error::Invalid);
        }
        let clusters = (total - data_sector) / u64::from(per_cluster);
        let fat_type = match clusters {
            0 => return Err(Error::Invalid),
            1..4085 => FatType::Fat12,
            4085..65525 => FatType::Fat16,
            _ => FatType::Fat32,
        };
        // The cluster count decides the type, and the BPB must be laid
        // out for it.
        let fat32 = fat_type == FatType::Fat32;
        if fat32 != fat32_layout || fat32 && (root_entries != 0 || total16 != 0) {
            return Err(Error::Invalid);
        }
        if !fat32 && root_entries == 0 || clusters > 0x0FFF_FFF5 {
            return Err(Error::Invalid);
        }
        let entry_bits = match fat_type {
            FatType::Fat12 => 12,
            FatType::Fat16 => 16,
            FatType::Fat32 => 32,
        };
        if (clusters + 2) * entry_bits > fat_sectors * s * 8 {
            return Err(Error::Invalid);
        }
        let (mut active_fat, mut root_cluster, mut fs_info) = (None, 0, None);
        if fat32 {
            let flags = u16_at(sector, 40);
            if flags & 0x80 != 0 {
                let active = (flags & 0xF) as u8;
                if active >= fats {
                    return Err(Error::Invalid);
                }
                active_fat = Some(active);
            }
            root_cluster = u32_at(sector, 44);
            if root_cluster < 2 || u64::from(root_cluster) >= clusters + 2 {
                return Err(Error::Invalid);
            }
            let info = u64::from(u16_at(sector, 48));
            if info != 0 && info != 0xFFFF && info < reserved {
                fs_info = Some(info * s);
            }
        }
        // The extended boot record, which signature 0x29 marks, holds a
        // label.
        let (signature, label_at) = if fat32 { (0x42, 0x47) } else { (0x26, 0x2B) };
        let mut boot_label = [0u8; 11];
        boot_label.copy_from_slice(&sector[label_at..label_at + 11]);
        let boot_label =
            (sector[signature] == 0x29 && &boot_label != b"NO NAME    ").then_some(boot_label);
        Ok(Layout {
            fat_type,
            sector_size,
            cluster_size,
            fat_start: reserved * s,
            fat_size: fat_sectors * s,
            fats,
            active_fat,
            root_start: (reserved + u64::from(fats) * fat_sectors) * s,
            root_entries,
            root_cluster,
            data_start: data_sector * s,
            clusters: clusters as u32,
            fs_info,
            total_bytes: total * s,
            boot_label,
        })
    }

    /// Where cluster `cluster`, at least 2, starts.
    pub fn cluster_start(&self, cluster: u32) -> u64 {
        self.data_start + u64::from(cluster - 2) * u64::from(self.cluster_size)
    }

    /// Whether `cluster` is one of the volume's data clusters.
    pub fn is_data(&self, cluster: u32) -> bool {
        cluster >= 2 && cluster < self.clusters + 2
    }
}
