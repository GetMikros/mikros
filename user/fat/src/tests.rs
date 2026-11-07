//! Tests against volumes that the `fatfs` crate formats and reads.

extern crate std;

use super::*;
use std::io::Cursor;
use std::vec;
use std::vec::Vec;

// Boot sectors.

/// A volume of `size` bytes and the type, with 512-byte clusters,
/// holding what `fill` puts there.
fn image(
    size: usize,
    fat_type: fatfs::FatType,
    fill: impl FnOnce(&fatfs::Dir<'_, Cursor<&mut Vec<u8>>>),
) -> Vec<u8> {
    let mut bytes = vec![0u8; size];
    let options = fatfs::FormatVolumeOptions::new()
        .fat_type(fat_type)
        .bytes_per_cluster(512);
    fatfs::format_volume(Cursor::new(&mut bytes), options).unwrap();
    {
        let fs = fatfs::FileSystem::new(Cursor::new(&mut bytes), fatfs::FsOptions::new()).unwrap();
        fill(&fs.root_dir());
        fs.unmount().unwrap();
    }
    bytes
}

const TYPES: [(usize, fatfs::FatType, FatType); 3] = [
    (1 << 20, fatfs::FatType::Fat12, FatType::Fat12),
    (8 << 20, fatfs::FatType::Fat16, FatType::Fat16),
    (40 << 20, fatfs::FatType::Fat32, FatType::Fat32),
];

#[test]
fn finds_the_layout_of_each_type() {
    for (size, ty, expected) in TYPES {
        let bytes = image(size, ty, |_| {});
        let layout = Layout::parse(&bytes[..512], size as u64).unwrap();
        assert_eq!(layout.fat_type, expected);
        assert_eq!(layout.cluster_size, 512);
        assert_eq!(layout.total_bytes, size as u64);
        assert!(layout.data_start + u64::from(layout.clusters) * 512 <= size as u64);
    }
}

#[test]
fn refuses_invalid_boot_sectors() {
    let size = 1 << 20;
    let bytes = image(size, fatfs::FatType::Fat12, |_| {});
    let parse = |b: &[u8], size: usize| Layout::parse(&b[..512], size as u64);
    assert!(parse(&bytes, size).is_ok());
    let mut broken = bytes.clone();
    broken[510] = 0;
    assert_eq!(parse(&broken, size), Err(Error::Invalid));
    let mut broken = bytes.clone();
    broken[11..13].copy_from_slice(&300u16.to_le_bytes());
    assert_eq!(parse(&broken, size), Err(Error::Invalid));
    let mut broken = bytes.clone();
    broken[16] = 0;
    assert_eq!(parse(&broken, size), Err(Error::Invalid));
    // A volume larger than its partition.
    assert_eq!(parse(&bytes, size / 2), Err(Error::Invalid));
}
