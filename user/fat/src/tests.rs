//! Tests against volumes that the `fatfs` crate formats and reads.

extern crate std;

use super::*;
use std::io::{Cursor, Read, Write};
use std::string::{String, ToString};
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

// Directories with short names.

struct Mem(Vec<u8>);

impl Device for Mem {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        assert!(offset % 512 == 0 && buf.len() % 512 == 0, "unaligned read");
        let at = offset as usize;
        let src = self.0.get(at..at + buf.len()).ok_or(Error::Io)?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), Error> {
        assert!(
            offset % 512 == 0 && data.len() % 512 == 0,
            "unaligned write"
        );
        let at = offset as usize;
        let dst = self.0.get_mut(at..at + data.len()).ok_or(Error::Io)?;
        dst.copy_from_slice(data);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

fn names<D: Device>(volume: &mut Volume<D>, dir: Dir) -> Vec<String> {
    let mut names: Vec<String> = volume
        .list(dir, 0, 1000)
        .unwrap()
        .0
        .into_iter()
        .map(|n| n.name)
        .collect();
    names.sort();
    names
}

#[test]
fn lists_and_finds_short_names() {
    for (size, ty, _) in TYPES {
        let bytes = image(size, ty, |root| {
            root.create_file("README.TXT").unwrap();
            root.create_dir("DOCS")
                .unwrap()
                .create_file("NOTES.TXT")
                .unwrap();
        });
        let mut volume = Volume::mount(Mem(bytes), size as u64, false).unwrap();
        let root = volume.root();
        assert_eq!(names(&mut volume, root), ["DOCS", "README.TXT"]);
        let docs = volume.find(root, "docs").unwrap();
        assert!(docs.is_directory());
        let docs = docs.as_dir(root).unwrap();
        assert_eq!(names(&mut volume, docs), ["NOTES.TXT"]);
        assert!(!volume.find(docs, "Notes.txt").unwrap().is_directory());
        assert_eq!(volume.find(root, "missing"), Err(Error::NotFound));
    }
}

#[test]
fn stamps_round_trip() {
    let stamp = Stamp::new(2026, 11, 7, 13, 30, 41, 250);
    assert_eq!(stamp.parts(), (2026, 11, 7, 13, 30, 41, 250));
}

// Long names and file contents.

fn sample(root: &fatfs::Dir<'_, Cursor<&mut Vec<u8>>>) {
    root.create_file("readme.txt")
        .unwrap()
        .write_all(b"hello")
        .unwrap();
    let docs = root.create_dir("Documents and Settings").unwrap();
    let big: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
    docs.create_file("A rather long file name.data")
        .unwrap()
        .write_all(&big)
        .unwrap();
    docs.create_dir("empty").unwrap();
}

fn read_all<D: Device>(volume: &mut Volume<D>, node: &mut Node) -> Vec<u8> {
    let mut data = vec![0u8; node.size() as usize + 10];
    let n = volume.read(node, 0, &mut data).unwrap();
    data.truncate(n);
    data
}

#[test]
fn reads_volumes_of_each_type() {
    for (size, ty, expected) in TYPES {
        let bytes = image(size, ty, sample);
        let mut volume = Volume::mount(Mem(bytes), size as u64, false).unwrap();
        assert_eq!(volume.layout().fat_type, expected);
        let root = volume.root();
        assert_eq!(
            names(&mut volume, root),
            ["Documents and Settings", "readme.txt"]
        );
        let mut readme = volume.find(root, "README.TXT").unwrap();
        assert_eq!(read_all(&mut volume, &mut readme), b"hello");
        let docs = volume.find(root, "documents AND settings").unwrap();
        let docs = docs.as_dir(root).unwrap();
        assert_eq!(
            names(&mut volume, docs),
            ["A rather long file name.data", "empty"]
        );
        let mut big = volume.find(docs, "a rather long file name.DATA").unwrap();
        let expected: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        assert_eq!(read_all(&mut volume, &mut big), expected);
        assert_eq!(volume.allocated(big.cluster()).unwrap(), 5120);
        assert_eq!(volume.allocated(0).unwrap(), 0);
        let offset = volume.entry_offset(&big).unwrap();
        assert!(offset >= volume.layout().data_start && offset % 32 == 0);
        let mut part = [0u8; 700];
        assert_eq!(volume.read(&mut big, 4600, &mut part).unwrap(), 400);
        assert_eq!(&part[..400], &expected[4600..]);
        assert_eq!(volume.find(root, "missing"), Err(Error::NotFound));
    }
}

// Writing files.

fn volume_bytes(volume: Volume<Mem>) -> Vec<u8> {
    volume.into_device().0
}

fn volume_bytes_ref(volume: &mut Volume<Mem>) -> Vec<u8> {
    volume.device().0.clone()
}

fn fatfs_read(bytes: &mut Vec<u8>, path: &str) -> Vec<u8> {
    let fs = fatfs::FileSystem::new(Cursor::new(bytes), fatfs::FsOptions::new()).unwrap();
    let mut data = Vec::new();
    fs.root_dir()
        .open_file(path)
        .unwrap()
        .read_to_end(&mut data)
        .unwrap();
    data
}

#[test]
fn keeps_the_clean_shutdown_bit() {
    let size = 8 << 20;
    let bytes = image(size, fatfs::FatType::Fat16, |_| {});
    let fat = |b: &[u8]| {
        let reserved = u16::from_le_bytes([b[14], b[15]]) as usize * 512;
        u16::from_le_bytes([b[reserved + 2], b[reserved + 3]])
    };
    assert_ne!(fat(&bytes) & 0x8000, 0);
    let mut volume = Volume::mount(Mem(bytes), size as u64, true).unwrap();
    let mounted = volume_bytes_ref(&mut volume);
    assert_eq!(fat(&mounted) & 0x8000, 0);
    // Mounted again without the bit, as after a crash: read-only.
    let crashed = Volume::mount(Mem(mounted), size as u64, true).unwrap();
    assert!(crashed.read_only());
    volume.unmount().unwrap();
    let bytes = volume_bytes(volume);
    assert_ne!(fat(&bytes) & 0x8000, 0);
}

/// Mounted for quick disconnection, the volume is marked as in use only
/// from the start of a change to its end, and a volume that was left in
/// use is still mounted read-only.
#[test]
fn marks_changes_in_progress_for_quick_disconnection() {
    let size = 8 << 20;
    let bytes = image(size, fatfs::FatType::Fat16, |_| {});
    let fat = |b: &[u8]| {
        let reserved = u16::from_le_bytes([b[14], b[15]]) as usize * 512;
        u16::from_le_bytes([b[reserved + 2], b[reserved + 3]])
    };
    let mut volume = Volume::mount_quick_disconnect(Mem(bytes), size as u64, true).unwrap();
    assert!(!volume.read_only());
    assert_ne!(fat(&volume_bytes_ref(&mut volume)) & 0x8000, 0);
    volume.begin_change().unwrap();
    let changing = volume_bytes_ref(&mut volume);
    assert_eq!(fat(&changing) & 0x8000, 0);
    let root = volume.root();
    let now = Stamp::new(2026, 11, 8, 12, 0, 0, 0);
    volume.create(root, "new.txt", false, now).unwrap();
    volume.end_change().unwrap();
    let between = volume_bytes_ref(&mut volume);
    assert_ne!(fat(&between) & 0x8000, 0);
    // Removed between changes: clean, and writable again.
    let again = Volume::mount_quick_disconnect(Mem(between), size as u64, true).unwrap();
    assert!(!again.read_only());
    // Removed during a change: read-only.
    let crashed = Volume::mount_quick_disconnect(Mem(changing), size as u64, true).unwrap();
    assert!(crashed.read_only());
}

#[test]
fn writes_files_that_fatfs_reads() {
    let now = Stamp::new(2026, 11, 7, 13, 30, 41, 250);
    for (size, ty, _) in TYPES {
        let bytes = image(size, ty, sample);
        let mut volume = Volume::mount(Mem(bytes), size as u64, true).unwrap();
        assert!(!volume.read_only());
        let root = volume.root();
        let mut readme = volume.find(root, "readme.txt").unwrap();
        let text: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        volume.write(&mut readme, 5, &text, now).unwrap();
        // A write past the end leaves zeros between.
        volume.write(&mut readme, 4000, b"tail", now).unwrap();
        let docs = volume
            .find(root, "Documents and Settings")
            .unwrap()
            .as_dir(root)
            .unwrap();
        let mut big = volume.find(docs, "A rather long file name.data").unwrap();
        volume.truncate(&mut big, 600, now).unwrap();
        volume.truncate(&mut big, 700, now).unwrap();
        volume.unmount().unwrap();
        let mut bytes = volume_bytes(volume);
        let readme = fatfs_read(&mut bytes, "readme.txt");
        assert_eq!(readme.len(), 4004);
        assert_eq!(&readme[..5], b"hello");
        assert_eq!(&readme[5..3005], &text[..]);
        assert!(readme[3005..4000].iter().all(|&b| b == 0));
        assert_eq!(&readme[4000..], b"tail");
        let big = fatfs_read(
            &mut bytes,
            "Documents and Settings/A rather long file name.data",
        );
        let expected: Vec<u8> = (0..600u32).map(|i| i as u8).collect();
        assert_eq!(big.len(), 700);
        assert_eq!(&big[..600], &expected[..]);
        assert!(big[600..].iter().all(|&b| b == 0));
    }
}

// Creating, removing and renaming.

/// What `fatfs` finds in the directory `path` of `bytes`: the long
/// names, sorted.
fn fatfs_names(bytes: &mut Vec<u8>, path: &str) -> Vec<String> {
    let fs = fatfs::FileSystem::new(Cursor::new(bytes), fatfs::FsOptions::new()).unwrap();
    let dir = if path.is_empty() {
        fs.root_dir()
    } else {
        fs.root_dir().open_dir(path).unwrap()
    };
    let mut names: Vec<String> = dir
        .iter()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n != "." && n != "..")
        .collect();
    names.sort();
    names
}

#[test]
fn writes_what_fatfs_reads() {
    let now = Stamp::new(2026, 11, 7, 13, 30, 41, 250);
    for (size, ty, _) in TYPES {
        let bytes = image(size, ty, sample);
        let mut volume = Volume::mount(Mem(bytes), size as u64, true).unwrap();
        assert!(!volume.read_only());
        let root = volume.root();
        let mut notes = volume
            .create(root, "Notes for Monday.txt", false, now)
            .unwrap();
        let text: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        volume.write(&mut notes, 0, &text, now).unwrap();
        // A write past the end leaves zeros between.
        volume.write(&mut notes, 4000, b"tail", now).unwrap();
        let dir = volume
            .create(root, "New Folder", true, now)
            .unwrap()
            .as_dir(root)
            .unwrap();
        for i in 0..40 {
            let name = std::format!("entry number {i} with a long name");
            volume.create(dir, &name, false, now).unwrap();
        }
        assert_eq!(
            volume
                .create(root, "notes FOR monday.TXT", false, now)
                .map(|_| ()),
            Err(Error::Exists)
        );
        assert_eq!(
            volume.create(root, "bad:name", false, now).map(|_| ()),
            Err(Error::Invalid)
        );
        let mut readme = volume.find(root, "readme.txt").unwrap();
        volume.truncate(&mut readme, 2, now).unwrap();
        volume
            .rename(root, "readme.txt", dir, "Read Me First.txt")
            .unwrap();
        volume
            .remove(dir, "entry number 3 with a long name", false)
            .unwrap();
        let docs = volume
            .find(root, "Documents and Settings")
            .unwrap()
            .as_dir(root)
            .unwrap();
        assert_eq!(
            volume.remove(root, "Documents and Settings", true),
            Err(Error::NotEmpty)
        );
        volume.remove(docs, "empty", true).unwrap();
        // Moving a directory updates its `..`.
        volume
            .rename(root, "Documents and Settings", dir, "Docs")
            .unwrap();
        assert_eq!(
            volume.rename(dir, "Docs", docs, "inside").map(|_| ()),
            Err(Error::Invalid)
        );
        // `..` of the moved directory names its new parent.
        let Dir::Cluster(moved) = volume.find(dir, "docs").unwrap().as_dir(root).unwrap() else {
            panic!("a directory has a cluster");
        };
        let at = volume.dot_dot_at(moved).unwrap();
        let dot_dot = crate::dir::Short::parse(&volume.read_slot(at).unwrap());
        assert_eq!(dot_dot.name, *b"..         ");
        assert_eq!(Dir::Cluster(dot_dot.cluster), dir);
        volume.unmount().unwrap();
        let mut bytes = volume_bytes(volume);

        let mut expected_root = vec!["New Folder".to_string(), "Notes for Monday.txt".to_string()];
        expected_root.sort();
        assert_eq!(fatfs_names(&mut bytes, ""), expected_root);
        let listed = fatfs_names(&mut bytes, "New Folder");
        assert_eq!(listed.len(), 39 + 2);
        assert!(listed.contains(&"Read Me First.txt".to_string()));
        assert!(listed.contains(&"Docs".to_string()));
        assert!(!listed.contains(&"entry number 3 with a long name".to_string()));
        let notes = fatfs_read(&mut bytes, "Notes for Monday.txt");
        assert_eq!(notes.len(), 4004);
        assert_eq!(&notes[..3000], &text[..]);
        assert!(notes[3000..4000].iter().all(|&b| b == 0));
        assert_eq!(&notes[4000..], b"tail");
        assert_eq!(
            fatfs_read(&mut bytes, "New Folder/Read Me First.txt"),
            b"he"
        );
        assert_eq!(
            fatfs_names(&mut bytes, "New Folder/Docs"),
            ["A rather long file name.data"]
        );
    }
}

#[test]
fn makes_unique_short_names() {
    let size = 1 << 20;
    let bytes = image(size, fatfs::FatType::Fat12, |_| {});
    let now = Stamp::new(2026, 1, 1, 0, 0, 0, 0);
    let mut volume = Volume::mount(Mem(bytes), size as u64, true).unwrap();
    let root = volume.root();
    for name in [
        "readme.txt",
        "Long File Name.txt",
        "Long File Number.txt",
        ".hidden",
        "a+b.c",
    ] {
        volume.create(root, name, false, now).unwrap();
    }
    volume.unmount().unwrap();
    let mut bytes = volume_bytes(volume);
    let fs = fatfs::FileSystem::new(Cursor::new(&mut bytes), fatfs::FsOptions::new()).unwrap();
    let mut shorts: Vec<String> = fs
        .root_dir()
        .iter()
        .map(|e| e.unwrap().short_file_name())
        .collect();
    shorts.sort();
    assert_eq!(
        shorts,
        [
            "A_B~1.C",
            "HIDDEN~1",
            "LONGFI~1.TXT",
            "LONGFI~2.TXT",
            "README.TXT"
        ]
    );
}
