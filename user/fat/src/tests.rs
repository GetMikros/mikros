//! Tests against volumes that the `fatfs` crate formats and reads.

extern crate std;

use super::*;
use std::io::Cursor;
use std::string::String;
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
