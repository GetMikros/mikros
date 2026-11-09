//! Tests against volumes that mkfs.exfat formats and fsck.exfat checks.

extern crate std;

use super::*;
use std::cell::Cell;
use std::format;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::string::String;
use std::sync::atomic::{AtomicU32, Ordering};
use std::vec;
use std::vec::Vec;

const MIB: usize = 1 << 20;

/// A volume in memory, which counts the writes to it.
struct Mem(Vec<u8>, Rc<Cell<u32>>);

impl Mem {
    fn new(bytes: Vec<u8>) -> Mem {
        Mem(bytes, Rc::new(Cell::new(0)))
    }
}

impl Device for Mem {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        assert!(offset % 512 == 0 && buf.len() % 512 == 0, "unaligned read");
        let at = offset as usize;
        buf.copy_from_slice(self.0.get(at..at + buf.len()).ok_or(Error::Io)?);
        Ok(())
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), Error> {
        assert!(
            offset % 512 == 0 && data.len() % 512 == 0,
            "unaligned write"
        );
        let at = offset as usize;
        self.0
            .get_mut(at..at + data.len())
            .ok_or(Error::Io)?
            .copy_from_slice(data);
        self.1.set(self.1.get() + 1);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

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

/// Checks the volume `bytes` with fsck.exfat, which must find it clean.
fn fsck(bytes: &[u8]) {
    let path = temp_path();
    std::fs::write(&path, bytes).unwrap();
    let out = Command::new("fsck.exfat")
        .arg("-n")
        .arg(&path)
        .output()
        .expect("fsck.exfat, from exfatprogs, runs");
    std::fs::remove_file(&path).unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "fsck.exfat: {text}");
}

fn names<D: Device>(volume: &mut Volume<D>, dir: Dir) -> Vec<String> {
    volume
        .list(dir, 0, 1000)
        .unwrap()
        .0
        .into_iter()
        .map(|n| n.name)
        .collect()
}

fn dirty(bytes: &[u8]) -> bool {
    bytes[106] & 2 != 0
}

#[test]
fn mounts_what_mkfs_formats() {
    for (size, args) in [
        (8 * MIB, &["-L", "STICK"][..]),
        (64 * MIB, &["-c", "32K", "-L", "Big One"][..]),
    ] {
        let bytes = mkfs(size, args);
        let mut volume = Volume::mount(Mem::new(bytes.clone()), size as u64, false).unwrap();
        let layout = volume.layout().clone();
        assert_eq!(layout.sector_size, 512);
        assert_eq!(layout.region, 0);
        assert_eq!(
            volume.label().unwrap().as_deref(),
            Some(args[args.len() - 1])
        );
        assert!(names(&mut volume, Dir::Root).is_empty());
        // Only the bitmap, the up-case table and the root are in use.
        let used = layout.clusters - volume.free_clusters();
        assert!((3..=6).contains(&used), "{used} clusters in use");
        assert!(volume.read_only());
    }
    assert_eq!(
        Volume::mount(Mem::new(vec![0u8; 8 * MIB]), 8 << 20, false).err(),
        Some(Error::Invalid)
    );
}

#[test]
fn marks_the_volume_dirty_while_mounted_for_writing() {
    let bytes = mkfs(8 * MIB, &[]);
    let size = bytes.len() as u64;
    let mut volume = Volume::mount(Mem::new(bytes), size, true).unwrap();
    assert!(!volume.read_only());
    assert!(dirty(&volume.device().0));
    volume.unmount().unwrap();
    assert!(volume.read_only());
    let bytes = volume.into_device().0;
    assert!(!dirty(&bytes));
    fsck(&bytes);
    // Quick disconnect marks it only between begin_change and end_change.
    let mut volume = Volume::mount_quick_disconnect(Mem::new(bytes), size, true).unwrap();
    assert!(!dirty(&volume.device().0));
    volume.begin_change().unwrap();
    assert!(dirty(&volume.device().0));
    volume.end_change().unwrap();
    assert!(!dirty(&volume.device().0));
}

#[test]
fn mounts_a_dirty_volume_read_only_without_writing() {
    let mut bytes = mkfs(8 * MIB, &[]);
    bytes[106] |= 2;
    let size = bytes.len() as u64;
    let device = Mem::new(bytes.clone());
    let writes = device.1.clone();
    let mut volume = Volume::mount(device, size, true).unwrap();
    assert!(volume.read_only());
    volume.begin_change().unwrap();
    volume.end_change().unwrap();
    volume.unmount().unwrap();
    assert_eq!(writes.get(), 0);
    assert_eq!(volume.into_device().0, bytes);
}

#[test]
fn falls_back_to_the_backup_boot_region_read_only() {
    let mut bytes = mkfs(8 * MIB, &["-L", "BACKUP"]);
    // A byte of the boot code, which the checksum covers.
    bytes[200] ^= 0xFF;
    let size = bytes.len() as u64;
    let device = Mem::new(bytes.clone());
    let writes = device.1.clone();
    let mut volume = Volume::mount(device, size, true).unwrap();
    assert_eq!(volume.layout().region, 12);
    assert!(volume.read_only());
    assert_eq!(volume.label().unwrap().as_deref(), Some("BACKUP"));
    volume.unmount().unwrap();
    assert_eq!(writes.get(), 0);
    // Without a valid backup either, there is no volume.
    bytes[12 * 512 + 200] ^= 0xFF;
    assert_eq!(
        Volume::mount(Mem::new(bytes), size, false).err(),
        Some(Error::Invalid)
    );
}

#[test]
fn finds_nothing_on_an_empty_volume() {
    let bytes = mkfs(8 * MIB, &[]);
    let size = bytes.len() as u64;
    let mut volume = Volume::mount(Mem::new(bytes), size, false).unwrap();
    // The root holds only the bitmap, the up-case table and the label.
    assert_eq!(volume.list(Dir::Root, 0, 10).unwrap().0.len(), 0);
    let missing = volume.find(Dir::Root, "anything");
    assert_eq!(missing.err(), Some(Error::NotFound));
    let too_long = volume.find(Dir::Root, &"x".repeat(256));
    assert_eq!(too_long.err(), Some(Error::NameTooLong));
    // A directory that no listing or finding gave is unknown.
    assert_eq!(volume.list(Dir::Sub(5), 0, 10).err(), Some(Error::NotFound));
}
