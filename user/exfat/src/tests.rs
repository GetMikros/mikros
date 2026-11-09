//! Tests against volumes that mkfs.exfat formats.

extern crate std;

use super::*;
use std::format;
use std::path::PathBuf;
use std::process::Command;
use std::string::String;
use std::sync::atomic::{AtomicU32, Ordering};
use std::vec;
use std::vec::Vec;

const MIB: usize = 1 << 20;

/// A file of its own in the temporary directory.
fn temp_path() -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("mikros-exfat-{}-{n}.img", std::process::id()))
}

/// A volume of `size` bytes that mkfs.exfat formats with `args`.
fn mkfs(size: usize, args: &[&str]) -> Vec<u8> {
    let path = temp_path();
    std::fs::write(&path, vec![0u8; size]).unwrap();
    let out = Command::new("mkfs.exfat")
        .args(args)
        .arg(&path)
        .output()
        .expect("mkfs.exfat, from exfatprogs, runs");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(
        out.status.success(),
        "mkfs.exfat: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    bytes
}

/// The main boot region of `bytes`, at the largest sector size.
fn main_region(bytes: &[u8]) -> &[u8] {
    &bytes[..12 * 4096]
}

#[test]
fn finds_the_layout_that_mkfs_writes() {
    for (size, args, cluster) in [
        (8 * MIB, &[][..], 4096),
        (64 * MIB, &["-c", "32K"][..], 32768),
    ] {
        let bytes = mkfs(size, args);
        let layout = Layout::parse(main_region(&bytes), 0, size as u64).unwrap();
        assert_eq!((layout.sector_size, layout.cluster_size), (512, cluster));
        assert_eq!(
            (layout.total_bytes, layout.region, layout.active),
            (size as u64, 0, 0)
        );
        assert!(layout.is_data(layout.root_cluster));
        assert!(!layout.is_data(1) && !layout.is_data(layout.clusters + 2));
        let heap_end = layout.cluster_start(layout.clusters + 2);
        assert!(layout.fat_start < layout.heap_start && heap_end <= size as u64);
        // A partition smaller than the volume does not hold it.
        let smaller = Layout::parse(main_region(&bytes), 0, size as u64 - 512);
        assert_eq!(smaller, Err(Error::Invalid));
    }
}

#[test]
fn checks_the_boot_region_and_finds_its_backup() {
    let mut bytes = mkfs(8 * MIB, &[]);
    let size = bytes.len() as u64;
    // The volume flags and the share in use lie outside the checksum.
    bytes[106] ^= 2;
    bytes[112] = 50;
    assert!(Layout::parse(main_region(&bytes), 0, size).is_ok());
    // A byte of the boot code lies inside it.
    bytes[200] ^= 0xFF;
    assert_eq!(
        Layout::parse(main_region(&bytes), 0, size),
        Err(Error::Invalid)
    );
    let backup = Layout::parse(&bytes[12 * 512..24 * 512], 12, size).unwrap();
    assert_eq!((backup.region, backup.sector_size), (12, 512));
    assert_eq!(
        Layout::parse(&[0u8; 12 * 512], 0, size),
        Err(Error::Invalid)
    );
    assert_eq!(Layout::parse(&bytes[..100], 0, size), Err(Error::Invalid));
}
