//! Finding the file system that a partition holds from the signature in
//! its first bytes, and the identifier that names it: btrfs, exFAT,
//! NTFS, FAT and ext, tried in that order.

use alloc::format;
use alloc::string::String;

/// How many bytes from the start of a partition the signatures lie in.
pub const PROBE_BYTES: usize = 128 * 1024;

/// The hardware IDs of the file systems whose first sector is a boot
/// sector, which a disk without a partition table may start with.
pub const BOOT_SECTOR_IDS: [&str; 3] = ["fs:fat", "fs:exfat", "fs:ntfs"];

/// A file system found by its signature: its hardware ID, such as
/// `fs:btrfs`, and its identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub id: &'static str,
    pub identifier: String,
}

fn found(id: &'static str, identifier: String) -> Option<Found> {
    Some(Found { id, identifier })
}

/// The bytes of `bytes` from `at` for `len`, when it holds them.
fn at(bytes: &[u8], at: usize, len: usize) -> Option<&[u8]> {
    bytes.get(at..at.checked_add(len)?)
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(at(bytes, offset, 4)?.try_into().ok()?))
}

/// A 32-bit volume serial number as lowercase hexadecimal digits, the
/// upper and lower 16 bits apart.
fn serial(value: u32) -> String {
    format!("{:04x}-{:04x}", value >> 16, value & 0xFFFF)
}

/// The 16 bytes of a UUID as it is written, lowercase with hyphens.
fn uuid(b: &[u8]) -> String {
    let hex = |range: core::ops::Range<usize>| {
        b[range]
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect::<String>()
    };
    format!(
        "{}-{}-{}-{}-{}",
        hex(0..4),
        hex(4..6),
        hex(6..8),
        hex(8..10),
        hex(10..16)
    )
}

/// The file system whose signature the first bytes `bytes` of a
/// partition of `partition_bytes` bytes hold, the first that matches, or
/// `None`. `bytes` holds up to `PROBE_BYTES` bytes, fewer for a smaller
/// partition.
pub fn find(bytes: &[u8], partition_bytes: u64) -> Option<Found> {
    // The superblock at 64 KiB, with its magic at 0x40 and the file
    // system UUID at 0x20.
    if at(bytes, 0x10040, 8) == Some(b"_BHRfS_M") {
        return found("fs:btrfs", uuid(at(bytes, 0x10020, 16)?));
    }
    let oem = at(bytes, 3, 8);
    if oem == Some(b"EXFAT   ") {
        return found("fs:exfat", serial(u32_at(bytes, 100)?));
    }
    if oem == Some(b"NTFS    ") {
        let value = u64::from_le_bytes(at(bytes, 0x48, 8)?.try_into().ok()?);
        return found("fs:ntfs", format!("{value:016x}"));
    }
    if let Some(sector) = at(bytes, 0, 512)
        && let Ok(layout) = mikros_fat::Layout::parse(sector, partition_bytes)
    {
        // The extended boot record of FAT32 lies further on than that of
        // FAT12 and FAT16.
        let id_at = match layout.fat_type {
            mikros_fat::FatType::Fat32 => 67,
            _ => 39,
        };
        return found("fs:fat", serial(u32_at(bytes, id_at)?));
    }
    // The superblock at 1 KiB, with its magic at 0x38 and its UUID at
    // 0x68.
    if at(bytes, 1080, 2) == Some(&[0x53, 0xEF]) {
        return found("fs:ext", uuid(at(bytes, 1024 + 0x68, 16)?));
    }
    None
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn image() -> Vec<u8> {
        vec![0u8; PROBE_BYTES]
    }

    #[test]
    fn finds_btrfs_by_its_superblock() {
        let mut bytes = image();
        bytes[0x10040..0x10048].copy_from_slice(b"_BHRfS_M");
        let fsid: Vec<u8> = (0u8..16).collect();
        bytes[0x10020..0x10030].copy_from_slice(&fsid);
        assert_eq!(
            find(&bytes, 1 << 30),
            Some(Found {
                id: "fs:btrfs",
                identifier: "00010203-0405-0607-0809-0a0b0c0d0e0f".into()
            })
        );
    }

    #[test]
    fn finds_exfat_and_ntfs_by_their_names() {
        let mut bytes = image();
        bytes[3..11].copy_from_slice(b"EXFAT   ");
        bytes[100..104].copy_from_slice(&0x1a2b_3c4du32.to_le_bytes());
        bytes[510] = 0x55;
        bytes[511] = 0xAA;
        assert_eq!(
            find(&bytes, 1 << 30).unwrap(),
            Found {
                id: "fs:exfat",
                identifier: "1a2b-3c4d".into()
            }
        );
        bytes[3..11].copy_from_slice(b"NTFS    ");
        bytes[0x48..0x50].copy_from_slice(&0x0123_4567_89ab_cdefu64.to_le_bytes());
        assert_eq!(
            find(&bytes, 1 << 30).unwrap(),
            Found {
                id: "fs:ntfs",
                identifier: "0123456789abcdef".into()
            }
        );
    }

    #[test]
    fn finds_fat_by_a_valid_boot_sector() {
        let size = 16 << 20;
        let mut disk = std::io::Cursor::new(vec![0u8; size]);
        let options = fatfs::FormatVolumeOptions::new().volume_id(0xcafe_f00d);
        fatfs::format_volume(&mut disk, options).unwrap();
        let disk = disk.into_inner();
        let found = find(&disk[..PROBE_BYTES], size as u64).unwrap();
        assert_eq!(
            found,
            Found {
                id: "fs:fat",
                identifier: "cafe-f00d".into()
            }
        );
        // Only the signature at 510 is not enough.
        let mut bytes = image();
        bytes[510] = 0x55;
        bytes[511] = 0xAA;
        assert_eq!(find(&bytes, 1 << 30), None);
    }

    #[test]
    fn finds_ext_last_and_nothing_in_zeros() {
        let mut bytes = image();
        bytes[1080] = 0x53;
        bytes[1081] = 0xEF;
        bytes[1024 + 0x68..1024 + 0x78].copy_from_slice(&[0xab; 16]);
        assert_eq!(find(&bytes, 1 << 30).unwrap().id, "fs:ext");
        assert_eq!(find(&image(), 1 << 30), None);
        // A partition smaller than the probe reads only what it has.
        assert_eq!(find(&bytes[..2048], 2048).unwrap().id, "fs:ext");
        assert_eq!(find(&[], 0), None);
    }
}
