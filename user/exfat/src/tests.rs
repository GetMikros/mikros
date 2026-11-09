//! Tests against volumes that mkfs.exfat formats and fsck.exfat checks.
//! Files are put on them through `Builder`, which writes entry sets,
//! FAT chains and the bitmap as an exFAT implementation would; fsck.exfat
//! checks what it wrote before the volume is read.

extern crate std;

use super::*;
use crate::entry::{ALLOCATION_POSSIBLE, NO_FAT_CHAIN, Set};
use crate::upcase::Upcase;
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

fn units(name: &str) -> Vec<u16> {
    name.encode_utf16().collect()
}

/// A timestamp of 2024-05-06 07:08:10 local time, 2 hours ahead of UTC.
fn stamp() -> Stamp {
    let dos = (44 << 25) | (5 << 21) | (6 << 16) | (7 << 11) | (8 << 5) | 5;
    Stamp {
        timestamp: dos,
        increment: 0,
        utc_offset: 0x80 | 8,
    }
}

/// Puts files and directories on a volume that mkfs.exfat formatted.
struct Builder {
    bytes: Vec<u8>,
    layout: Layout,
    upcase: Upcase,
    /// Where the allocation bitmap starts.
    bitmap: usize,
}

impl Builder {
    fn new(bytes: Vec<u8>) -> Builder {
        let layout = Layout::parse(&bytes[..12 * 4096], 0, bytes.len() as u64).unwrap();
        let root = layout.cluster_start(layout.root_cluster) as usize;
        let (mut bitmap, mut table) = (0, Vec::new());
        for e in bytes[root..root + layout.cluster_size as usize].chunks(32) {
            let (first, len) = entry::extent(e);
            let at = layout.cluster_start(first.max(2)) as usize;
            match e[0] {
                entry::BITMAP => bitmap = at,
                entry::UPCASE => table = bytes[at..at + len as usize].to_vec(),
                _ => {}
            }
        }
        Builder {
            upcase: Upcase::parse(&table),
            bytes,
            layout,
            bitmap,
        }
    }

    fn used(&self, c: u32) -> bool {
        let i = (c - 2) as usize;
        self.bytes[self.bitmap + i / 8] & (1 << (i % 8)) != 0
    }

    fn mark(&mut self, c: u32) {
        let i = (c - 2) as usize;
        self.bytes[self.bitmap + i / 8] |= 1 << (i % 8);
    }

    /// Takes `count` free clusters, in a row when `step` is 0 and
    /// otherwise each `step` on from the last one taken or further, and
    /// marks them in the bitmap.
    fn take(&mut self, count: usize, step: u32) -> Vec<u32> {
        if step == 0 {
            let first = (2..)
                .find(|&c| (c..c + count as u32).all(|c| !self.used(c)))
                .unwrap();
            let taken: Vec<u32> = (first..first + count as u32).collect();
            taken.iter().for_each(|&c| self.mark(c));
            return taken;
        }
        let mut taken = Vec::new();
        let mut c = 2;
        while taken.len() < count {
            if self.used(c) {
                c += 1;
                continue;
            }
            self.mark(c);
            taken.push(c);
            c += step;
        }
        taken
    }

    fn set_fat(&mut self, c: u32, value: u32) {
        let at = self.layout.fat_start as usize + 4 * c as usize;
        self.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn cluster(&mut self, c: u32) -> &mut [u8] {
        let at = self.layout.cluster_start(c) as usize;
        &mut self.bytes[at..at + self.layout.cluster_size as usize]
    }

    /// Adds the entry set `set` to the directory whose single cluster, or
    /// first one for the root, is `dir`, and gives the slot of its file
    /// entry.
    fn add(&mut self, dir: u32, set: &Set) -> u32 {
        let encoded = set.encode(&self.upcase);
        let entries = self.cluster(dir);
        let slot = (0..entries.len() / 32)
            .find(|&s| entries[s * 32] == entry::END)
            .unwrap();
        entries[slot * 32..slot * 32 + encoded.len()].copy_from_slice(&encoded);
        slot as u32
    }

    /// Adds a vendor extension entry to the set whose file entry is at
    /// `slot` of `dir`, the last set added there.
    fn extend(&mut self, dir: u32, slot: u32) {
        let entries = self.cluster(dir);
        let at = slot as usize * 32;
        let end = at + (usize::from(entries[at + 1]) + 1) * 32;
        entries[end..end + 32].fill(0);
        entries[end] = 0xE0;
        entries[at + 1] += 1;
        let sum = entry::set_checksum(&entries[at..end + 32]);
        entries[at + 2..at + 4].copy_from_slice(&sum.to_le_bytes());
    }

    /// Puts a file named `name` holding `data`, of which `valid` bytes
    /// are valid, in `dir`: in clusters in a row without a chain when
    /// `step` is 0, and otherwise in a chain of clusters `step` apart.
    fn file(&mut self, dir: u32, name: &str, data: &[u8], valid: usize, step: u32) -> u32 {
        let size = self.layout.cluster_size as usize;
        let clusters = self.take(data.len().div_ceil(size), step);
        for (i, &c) in clusters.iter().enumerate() {
            let part = &data[i * size..((i + 1) * size).min(data.len())];
            self.cluster(c)[..part.len()].copy_from_slice(part);
            if step > 0 {
                let next = clusters.get(i + 1).copied().unwrap_or(volume::END_OF_CHAIN);
                self.set_fat(c, next);
            }
        }
        let set = Set {
            attributes: ARCHIVE,
            created: stamp(),
            modified: stamp(),
            accessed: stamp(),
            // fsck.exfat wants an empty file to have no flag for clusters
            // in a row.
            flags: ALLOCATION_POSSIBLE
                | if step == 0 && !data.is_empty() {
                    NO_FAT_CHAIN
                } else {
                    0
                },
            valid_size: valid as u64,
            first_cluster: clusters.first().copied().unwrap_or(0),
            size: data.len() as u64,
            name: units(name),
        };
        self.add(dir, &set)
    }

    /// Puts an empty directory of one cluster named `name` in `dir`, and
    /// gives its cluster.
    fn dir(&mut self, dir: u32, name: &str) -> u32 {
        let c = self.take(1, 0)[0];
        self.cluster(c).fill(0);
        let size = u64::from(self.layout.cluster_size);
        let set = Set {
            attributes: DIRECTORY,
            created: stamp(),
            modified: stamp(),
            accessed: stamp(),
            flags: ALLOCATION_POSSIBLE | NO_FAT_CHAIN,
            valid_size: size,
            first_cluster: c,
            size,
            name: units(name),
        };
        self.add(dir, &set);
        c
    }
}

/// The bytes 0 to 255 over and over, `len` of them.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed))
        .collect()
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

fn read_all<D: Device>(volume: &mut Volume<D>, node: &mut Node) -> Vec<u8> {
    let mut data = vec![0u8; node.size() as usize];
    assert_eq!(volume.read(node, 0, &mut data).unwrap(), data.len());
    data
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
fn reads_files_in_a_row_and_in_chains() {
    let mut b = Builder::new(mkfs(8 * MIB, &[]));
    let cluster = b.layout.cluster_size as usize;
    let root = b.layout.root_cluster;
    let row = pattern(3 * cluster + 100, 1);
    let chained = pattern(4 * cluster + 7, 2);
    let half = pattern(2 * cluster, 3);
    b.file(root, "Row.bin", &row, row.len(), 0);
    b.file(root, "Chained.bin", &chained, chained.len(), 2);
    b.file(root, "Half valid.bin", &half, cluster + 10, 0);
    b.file(root, "empty", &[], 0, 0);
    let sub = b.dir(root, "Sub Dir");
    b.file(sub, "inner.txt", b"hello from inside", 17, 1);
    fsck(&b.bytes);

    let size = b.bytes.len() as u64;
    let mut volume = Volume::mount(Mem::new(b.bytes), size, false).unwrap();
    assert_eq!(
        names(&mut volume, Dir::Root),
        [
            "Row.bin",
            "Chained.bin",
            "Half valid.bin",
            "empty",
            "Sub Dir"
        ]
    );
    for (name, data) in [("row.BIN", &row), ("CHAINED.bin", &chained)] {
        let mut node = volume.find(Dir::Root, name).unwrap();
        assert_eq!(read_all(&mut volume, &mut node), *data);
        // Reads that cross clusters, backwards and forwards.
        for offset in [cluster as u64 * 3 + 5, cluster as u64 - 3, 1] {
            let mut buf = [0u8; 300];
            let n = volume.read(&mut node, offset, &mut buf).unwrap();
            assert_eq!(buf[..n], data[offset as usize..offset as usize + n]);
        }
        assert_eq!(
            volume
                .read(&mut node, data.len() as u64, &mut [0u8; 8])
                .unwrap(),
            0
        );
    }
    let chained_node = volume.find(Dir::Root, "chained.bin").unwrap();
    assert_eq!(volume.allocated(&chained_node).unwrap(), 5 * cluster as u64);
    let mut node = volume.find(Dir::Root, "half valid.bin").unwrap();
    let data = read_all(&mut volume, &mut node);
    assert_eq!(data[..cluster + 10], half[..cluster + 10]);
    assert!(data[cluster + 10..].iter().all(|&b| b == 0));
    let mut empty = volume.find(Dir::Root, "EMPTY").unwrap();
    assert_eq!((empty.size(), volume.allocated(&empty).unwrap()), (0, 0));
    assert_eq!(volume.read(&mut empty, 0, &mut [0u8; 8]).unwrap(), 0);

    let dir = volume.find(Dir::Root, "sub dir").unwrap();
    assert!(dir.is_directory());
    assert_eq!(dir.attributes() & DIRECTORY, DIRECTORY);
    assert_eq!(dir.modified(), stamp());
    let sub = dir.as_dir().unwrap();
    assert_eq!(volume.allocated_dir(sub).unwrap(), cluster as u64);
    let mut inner = volume.find(sub, "Inner.TXT").unwrap();
    assert_eq!(read_all(&mut volume, &mut inner), b"hello from inside");
    let offset = volume.entry_offset(&inner).unwrap();
    assert_eq!(offset, volume.layout().cluster_start(dir.set.first_cluster));
    assert_eq!(volume.find(sub, "missing").err(), Some(Error::NotFound));
    assert_eq!(
        volume.read(&mut dir.clone(), 0, &mut [0u8; 8]).err(),
        Some(Error::IsDirectory)
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

#[test]
fn finds_entries_in_subdirectories() {
    let mut b = Builder::new(mkfs(8 * MIB, &[]));
    let root = b.layout.root_cluster;
    let sub = b.dir(root, "Sub Dir");
    let deeper = b.dir(sub, "deeper");
    b.file(deeper, "leaf.txt", b"leaf", 4, 0);
    fsck(&b.bytes);
    let size = b.bytes.len() as u64;
    let mut volume = Volume::mount(Mem::new(b.bytes), size, false).unwrap();
    let node = volume.find(Dir::Root, "SUB DIR").unwrap();
    assert!(node.is_directory() && node.attributes() & DIRECTORY != 0);
    assert_eq!((node.modified(), node.created()), (stamp(), stamp()));
    let sub = node.as_dir().unwrap();
    let cluster = u64::from(volume.layout().cluster_size);
    assert_eq!(volume.allocated_dir(sub).unwrap(), cluster);
    assert_eq!(names(&mut volume, sub), ["deeper"]);
    let deeper = volume.find(sub, "Deeper").unwrap().as_dir().unwrap();
    let leaf = volume.find(deeper, "LEAF.TXT").unwrap();
    assert_eq!((leaf.name.as_str(), leaf.size()), ("leaf.txt", 4));
    assert!(leaf.as_dir().is_none());
    let Dir::Sub(first) = deeper else {
        panic!("{deeper:?} is the root");
    };
    let offset = volume.entry_offset(&leaf).unwrap();
    assert_eq!(offset, volume.layout().cluster_start(first));
    assert_eq!(volume.find(deeper, "leaf").err(), Some(Error::NotFound));
}

#[test]
fn compares_names_through_the_up_case_table() {
    let mut b = Builder::new(mkfs(8 * MIB, &[]));
    let root = b.layout.root_cluster;
    let long = "A name that needs three file name entries.txt";
    b.file(root, "Ärger über Öl.txt", b"1", 1, 0);
    b.file(root, long, b"2", 1, 0);
    fsck(&b.bytes);
    let size = b.bytes.len() as u64;
    let mut volume = Volume::mount(Mem::new(b.bytes), size, false).unwrap();
    let node = volume.find(Dir::Root, "ärger ÜBER öl.TXT").unwrap();
    assert_eq!(node.name, "Ärger über Öl.txt");
    assert_eq!(
        volume.find(Dir::Root, &long.to_lowercase()).unwrap().name,
        long
    );
    // A name that a single unit of differs in is another name.
    assert_eq!(
        volume.find(Dir::Root, "Arger über Öl.txt").err(),
        Some(Error::NotFound)
    );
    let too_long = "x".repeat(256);
    assert_eq!(
        volume.find(Dir::Root, &too_long).err(),
        Some(Error::NameTooLong)
    );
    assert_eq!(
        volume.find(Dir::Root, &too_long[..255]).err(),
        Some(Error::NotFound)
    );
}

#[test]
fn lists_in_steps_and_leaves_out_damaged_sets() {
    let mut b = Builder::new(mkfs(8 * MIB, &[]));
    let root = b.layout.root_cluster;
    let mut slots = Vec::new();
    for i in 0..5 {
        slots.push(b.file(root, &format!("file {i}"), b"x", 1, 0));
        // The set of "file 1" holds one more secondary entry than its
        // name needs, which fsck.exfat 1.2.0 does not take.
        if i == 1 {
            b.extend(root, slots[1]);
        }
    }
    // A wrong checksum in the set of "file 2".
    let at = b.layout.cluster_start(root) as usize + slots[2] as usize * 32;
    b.bytes[at + 2] ^= 1;
    let size = b.bytes.len() as u64;
    let mut volume = Volume::mount(Mem::new(b.bytes), size, false).unwrap();
    let (first, next) = volume.list(Dir::Root, 0, 2).unwrap();
    let first: Vec<String> = first.into_iter().map(|n| n.name).collect();
    assert_eq!(first, ["file 0", "file 1"]);
    let (rest, end) = volume.list(Dir::Root, next, 10).unwrap();
    let rest: Vec<String> = rest.into_iter().map(|n| n.name).collect();
    assert_eq!(rest, ["file 3", "file 4"]);
    assert!(volume.list(Dir::Root, end, 10).unwrap().0.is_empty());
    assert_eq!(
        volume.find(Dir::Root, "file 2").err(),
        Some(Error::NotFound)
    );
}

// Writing.

fn mount_writable(bytes: Vec<u8>) -> Volume<Mem> {
    let size = bytes.len() as u64;
    Volume::mount(Mem::new(bytes), size, true).unwrap()
}

/// Unmounts `volume`, checks it with fsck.exfat, and mounts it again
/// read-only.
fn remount(mut volume: Volume<Mem>) -> Volume<Mem> {
    volume.unmount().unwrap();
    let bytes = volume.into_device().0;
    fsck(&bytes);
    let size = bytes.len() as u64;
    Volume::mount(Mem::new(bytes), size, false).unwrap()
}

#[test]
fn truncates_files_in_a_row_and_in_chains() {
    let mut b = Builder::new(mkfs(8 * MIB, &[]));
    let root = b.layout.root_cluster;
    b.file(root, "a", &[], 0, 0);
    b.file(root, "b", &[], 0, 0);
    let mut volume = mount_writable(b.bytes);
    let cluster = u64::from(volume.layout().cluster_size);
    let free = volume.free_clusters();
    let mut a = volume.find(Dir::Root, "a").unwrap();
    let mut b = volume.find(Dir::Root, "b").unwrap();
    volume.truncate(&mut a, 2 * cluster + 1, stamp()).unwrap();
    assert!(a.set.contiguous());
    // Growing while the clusters after it are free keeps the row.
    volume.truncate(&mut a, 4 * cluster, stamp()).unwrap();
    assert!(a.set.contiguous());
    // Once another file takes the cluster after it, it needs a chain.
    volume.truncate(&mut b, 1, stamp()).unwrap();
    volume.truncate(&mut a, 6 * cluster, stamp()).unwrap();
    assert!(!a.set.contiguous());
    assert_eq!(volume.allocated(&a).unwrap(), 6 * cluster);
    assert_eq!(volume.free_clusters(), free - 7);
    // The bytes it gained lie past its valid data.
    assert!(read_all(&mut volume, &mut a).iter().all(|&x| x == 0));
    volume.truncate(&mut a, 2 * cluster, stamp()).unwrap();
    assert_eq!(volume.free_clusters(), free - 3);
    volume.truncate(&mut b, 0, stamp()).unwrap();
    assert_eq!((volume.allocated(&b).unwrap(), b.set.first_cluster), (0, 0));
    // Two seconds later, in UTC.
    let later = Stamp {
        timestamp: stamp().timestamp + 1,
        increment: 0,
        utc_offset: 0x80,
    };
    volume.set_times(&mut b, Some(later), Some(later)).unwrap();
    let mut volume = remount(volume);
    assert_eq!(volume.free_clusters(), free - 2);
    let a = volume.find(Dir::Root, "A").unwrap();
    assert_eq!((a.size(), a.modified()), (2 * cluster, stamp()));
    let b = volume.find(Dir::Root, "b").unwrap();
    assert_eq!((b.size(), b.accessed(), b.modified()), (0, later, later));
}

#[test]
fn refuses_to_truncate_when_read_only_or_full() {
    let mut b = Builder::new(mkfs(8 * MIB, &[]));
    let root = b.layout.root_cluster;
    b.file(root, "f", &[], 0, 0);
    let size = b.bytes.len() as u64;
    let mut volume = Volume::mount(Mem::new(b.bytes), size, false).unwrap();
    let mut f = volume.find(Dir::Root, "f").unwrap();
    let refused = volume.truncate(&mut f, 10, stamp());
    assert_eq!(refused.err(), Some(Error::ReadOnly));
    let mut volume = mount_writable(volume.into_device().0);
    let mut f = volume.find(Dir::Root, "f").unwrap();
    let free = volume.free_clusters();
    let cluster = u64::from(volume.layout().cluster_size);
    let too_big = volume.truncate(&mut f, u64::from(free + 1) * cluster, stamp());
    assert_eq!(too_big.err(), Some(Error::NoSpace));
    assert_eq!(volume.free_clusters(), free);
    volume
        .truncate(&mut f, u64::from(free) * cluster, stamp())
        .unwrap();
    assert_eq!(volume.free_clusters(), 0);
    remount(volume);
}

#[test]
fn writes_files_and_zeroes_what_lies_before_their_data() {
    let mut volume = mount_writable(mkfs(8 * MIB, &[]));
    let cluster = volume.layout().cluster_size as usize;
    let mut a = volume.create(Dir::Root, "a.bin", false, stamp()).unwrap();
    let mut expected = pattern(2 * cluster + 10, 1);
    volume.write(&mut a, 0, &expected, stamp()).unwrap();
    let mut b = volume.create(Dir::Root, "b.bin", false, stamp()).unwrap();
    volume
        .write(&mut b, 0, &pattern(cluster, 3), stamp())
        .unwrap();
    // Past the end of a, across a chain that now starts.
    let more = pattern(2 * cluster, 4);
    volume
        .write(&mut a, expected.len() as u64, &more, stamp())
        .unwrap();
    expected.extend_from_slice(&more);
    assert!(!a.set.contiguous());
    // Over bytes in the middle.
    volume.write(&mut a, 5, b"middle", stamp()).unwrap();
    expected[5..11].copy_from_slice(b"middle");
    // Far past the valid data, after a truncate that left it short.
    let mut c = volume.create(Dir::Root, "c", false, stamp()).unwrap();
    volume.write(&mut c, 0, b"hello", stamp()).unwrap();
    volume.truncate(&mut c, cluster as u64, stamp()).unwrap();
    volume
        .write(&mut c, 3 * cluster as u64 + 5, b"world", stamp())
        .unwrap();
    let mut volume = remount(volume);
    let mut a = volume.find(Dir::Root, "a.bin").unwrap();
    assert_eq!(read_all(&mut volume, &mut a), expected);
    assert_eq!(a.modified(), stamp());
    let mut b = volume.find(Dir::Root, "b.bin").unwrap();
    assert_eq!(read_all(&mut volume, &mut b), pattern(cluster, 3));
    let mut c = volume.find(Dir::Root, "c").unwrap();
    let data = read_all(&mut volume, &mut c);
    assert_eq!(data.len(), 3 * cluster + 10);
    assert_eq!(
        (&data[..5], &data[data.len() - 5..]),
        (&b"hello"[..], &b"world"[..])
    );
    assert!(data[5..data.len() - 5].iter().all(|&x| x == 0));
}

#[test]
fn creates_files_and_directories() {
    let bytes = mkfs(8 * MIB, &[]);
    let size = bytes.len() as u64;
    let mut volume = Volume::mount(Mem::new(bytes), size, false).unwrap();
    let refused = volume.create(Dir::Root, "f", false, stamp());
    assert_eq!(refused.err(), Some(Error::ReadOnly));
    let mut volume = mount_writable(volume.into_device().0);
    let free = volume.free_clusters();
    let docs = volume.create(Dir::Root, "Docs", true, stamp()).unwrap();
    assert_eq!(volume.free_clusters(), free - 1);
    let docs = docs.as_dir().unwrap();
    let mut note = volume.create(docs, "note.txt", false, stamp()).unwrap();
    volume.write(&mut note, 0, b"note", stamp()).unwrap();
    let mut create = |name: &str| volume.create(Dir::Root, name, false, stamp()).err();
    assert_eq!(create("docs"), Some(Error::Exists));
    assert_eq!(create("bad:name"), Some(Error::Invalid));
    assert_eq!(create("trailing."), Some(Error::Invalid));
    assert_eq!(create(&"x".repeat(256)), Some(Error::NameTooLong));
    assert_eq!(create(&"x".repeat(255)), None);
    let mut volume = remount(volume);
    let docs = volume.find(Dir::Root, "docs").unwrap();
    assert_eq!(
        (docs.created(), docs.attributes() & DIRECTORY),
        (stamp(), DIRECTORY)
    );
    let docs = docs.as_dir().unwrap();
    let mut note = volume.find(docs, "NOTE.TXT").unwrap();
    assert_eq!(read_all(&mut volume, &mut note), b"note");
}

#[test]
fn grows_directories_a_cluster_at_a_time() {
    // With 512-byte clusters, a set of a long name takes more than one.
    for (args, files) in [(&["-c", "512"][..], 3), (&[][..], 100)] {
        let mut volume = mount_writable(mkfs(8 * MIB, args));
        let sub = volume.create(Dir::Root, "sub", true, stamp()).unwrap();
        let sub = sub.as_dir().unwrap();
        let name = |i: usize| format!("{i:03}{}", " long name".repeat(25 - i % 25));
        for dir in [Dir::Root, sub] {
            for i in 0..files {
                volume.create(dir, &name(i), false, stamp()).unwrap();
            }
        }
        let cluster = u64::from(volume.layout().cluster_size);
        assert!(volume.allocated_dir(sub).unwrap() > cluster);
        let mut volume = remount(volume);
        let sub = volume.find(Dir::Root, "sub").unwrap().as_dir().unwrap();
        let expected: Vec<String> = (0..files).map(name).collect();
        assert_eq!(names(&mut volume, sub), expected);
        assert_eq!(names(&mut volume, Dir::Root)[1..], expected);
    }
}

#[test]
fn removes_and_renames() {
    let mut volume = mount_writable(mkfs(8 * MIB, &[]));
    let free = volume.free_clusters();
    let docs = volume.create(Dir::Root, "Docs", true, stamp()).unwrap();
    let docs = docs.as_dir().unwrap();
    volume.create(docs, "note.txt", false, stamp()).unwrap();
    let mut top = volume.create(Dir::Root, "Top.txt", false, stamp()).unwrap();
    volume.write(&mut top, 0, b"top", stamp()).unwrap();
    let mut remove = |name: &str, directory: bool| volume.remove(Dir::Root, name, directory).err();
    assert_eq!(remove("docs", true), Some(Error::NotEmpty));
    assert_eq!(remove("top.txt", true), Some(Error::NotDirectory));
    assert_eq!(remove("docs", false), Some(Error::IsDirectory));
    assert_eq!(remove("missing", false), Some(Error::NotFound));
    // A new case of the same name.
    let renamed = volume
        .rename(Dir::Root, "Top.txt", Dir::Root, "TOP.txt")
        .unwrap();
    assert_eq!(renamed.name, "TOP.txt");
    assert_eq!(names(&mut volume, Dir::Root), ["Docs", "TOP.txt"]);
    // Into a directory, over a file there.
    let mut other = volume.create(docs, "other.txt", false, stamp()).unwrap();
    volume.write(&mut other, 0, b"replaced", stamp()).unwrap();
    volume
        .rename(Dir::Root, "top.txt", docs, "other.txt")
        .unwrap();
    assert_eq!(names(&mut volume, Dir::Root), ["Docs"]);
    assert_eq!(names(&mut volume, docs), ["note.txt", "other.txt"]);
    // A directory moves, but not below itself.
    let sub = volume.create(Dir::Root, "Sub", true, stamp()).unwrap();
    let sub = sub.as_dir().unwrap();
    volume.rename(Dir::Root, "Docs", sub, "Docs").unwrap();
    let into_itself = volume.rename(sub, "Docs", docs, "inner");
    assert_eq!(into_itself.err(), Some(Error::Invalid));
    volume
        .create(docs, "after the move", false, stamp())
        .unwrap();
    let mut volume = remount(volume);
    let sub = volume.find(Dir::Root, "sub").unwrap().as_dir().unwrap();
    let docs = volume.find(sub, "docs").unwrap().as_dir().unwrap();
    let expected = ["note.txt", "other.txt", "after the move"];
    assert_eq!(names(&mut volume, docs), expected);
    let mut other = volume.find(docs, "other.txt").unwrap();
    assert_eq!(read_all(&mut volume, &mut other), b"top");
    // Removing everything gives every cluster back.
    let mut volume = mount_writable(volume.into_device().0);
    let sub = volume.find(Dir::Root, "sub").unwrap().as_dir().unwrap();
    let docs = volume.find(sub, "docs").unwrap().as_dir().unwrap();
    for name in expected {
        volume.remove(docs, name, false).unwrap();
    }
    volume.remove(sub, "docs", true).unwrap();
    volume.remove(Dir::Root, "sub", true).unwrap();
    assert_eq!(volume.free_clusters(), free);
    let mut volume = remount(volume);
    assert!(names(&mut volume, Dir::Root).is_empty());
}
