//! Building a FAT32 EFI system partition in memory.

use anyhow::{Context, Result};
use std::io::{Cursor, Write};

/// The smallest partition size, which leaves enough clusters for FAT32.
const MIN_SIZE: u64 = 64 << 20;

/// The partition size for files of `total` bytes: a quarter more than the
/// files, plus 16 MiB for the file system and later changes, in whole MiB.
pub fn size_for(total: u64) -> u64 {
    let size = total + total / 4 + (16 << 20);
    size.max(MIN_SIZE).div_ceil(1 << 20) << 20
}

/// Formats a FAT32 file system of `size` bytes holding `files`, given as
/// paths with `/` separators and their contents, and returns its bytes.
pub fn build(size: u64, files: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    build_volume(size, fatfs::FatType::Fat32, *b"EFI SYSTEM ", files)
}

/// A FAT file system of `size` bytes and type `fat_type` with the volume
/// label `label`, holding `files`, as `build` makes the ESP.
pub fn build_volume(
    size: u64,
    fat_type: fatfs::FatType,
    label: [u8; 11],
    files: &[(&str, &[u8])],
) -> Result<Vec<u8>> {
    let mut disk = Cursor::new(vec![0u8; usize::try_from(size)?]);
    let options = fatfs::FormatVolumeOptions::new()
        .fat_type(fat_type)
        .volume_label(label);
    fatfs::format_volume(&mut disk, options).context("formatting the volume failed")?;
    {
        let fs = fatfs::FileSystem::new(&mut disk, fatfs::FsOptions::new())?;
        for (path, data) in files {
            let mut dir = fs.root_dir();
            let (parents, name) = path.rsplit_once('/').unwrap_or(("", path));
            for part in parents.split('/').filter(|p| !p.is_empty()) {
                dir = dir.create_dir(part)?;
            }
            let mut file = dir.create_file(name)?;
            file.truncate()?;
            file.write_all(data)
                .with_context(|| format!("writing {path} to the ESP failed"))?;
        }
        fs.unmount()?;
    }
    Ok(disk.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn sizes_partitions_in_whole_mebibytes() {
        assert_eq!(size_for(0), 64 << 20);
        assert_eq!(size_for(100 << 20), 141 << 20);
    }

    #[test]
    fn writes_files_in_nested_folders() {
        let files: [(&str, &[u8]); 3] = [
            ("EFI/Mikros/bootmenu.efi", b"menu"),
            ("EFI/BOOT/BOOTX64.EFI", b"fallback"),
            ("EFI/Mikros/boot.conf", b"root=bootfs"),
        ];
        let bytes = build(size_for(0), &files).unwrap();
        let fs = fatfs::FileSystem::new(Cursor::new(bytes), fatfs::FsOptions::new()).unwrap();
        assert_eq!(fs.fat_type(), fatfs::FatType::Fat32);
        for (path, data) in files {
            let mut read = Vec::new();
            fs.root_dir()
                .open_file(path)
                .unwrap()
                .read_to_end(&mut read)
                .unwrap();
            assert_eq!(read, data, "{path}");
        }
    }
}
