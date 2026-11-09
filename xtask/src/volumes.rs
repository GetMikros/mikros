//! The run of the test image in which the volumes of a USB disk come and
//! go in `/Volumes`: the USB disk holds a GPT with a basic data partition
//! with a FAT file system labelled `USBSTICK`, and a Mikros root type
//! partition with a btrfs labelled `USBBTRFS`, each with `HELLO.TXT` on
//! it. The test init serves `tests/volumes.img` with `test=usb-volumes`,
//! whose volumes probe reads and writes both volumes through `/Volumes`.
//! The runner then unplugs the disk through QEMU's monitor and waits for
//! the probe to see the volumes leave. Afterwards the FAT volume must hold
//! what the probe wrote.

use crate::build::{self, Kind};
use crate::image::{esp, gpt};
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;
use std::time::{Duration, Instant};

/// The lines the volumes probe logs once it has used the volume, once the
/// volume has gone, and when a check failed.
const READY: &str = "volumes-probe: read and wrote the USB volumes";
const GONE: &str = "volumes-probe: the USB volumes are gone";
const FAILED: &str = "volumes-probe: FAILED";
const PANIC_LINE: &str = "KERNEL PANIC: ";
/// The USB disk: the blocks of its FAT and btrfs partitions, its size in
/// sectors with room for the backup GPT, its GUIDs, the partition types,
/// and the QEMU device ID it is unplugged by.
const FIRST_LBA: u64 = 34;
const LAST_LBA: u64 = 2047;
const BTRFS_FIRST_LBA: u64 = 2048;
const BTRFS_SECTORS: u64 = 128 << 11;
const SECTORS: u64 = BTRFS_FIRST_LBA + BTRFS_SECTORS + 34;
const DISK_GUID: &str = "55534221-0000-4000-8000-0000000000d4";
const PARTITION_GUID: &str = "55534221-0000-4000-8000-000000000004";
const BTRFS_GUID: &str = "55534221-0000-4000-8000-000000000005";
const BASIC_DATA: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";
const MIKROS_ROOT: &str = "fa730af6-8ba3-42d9-b155-466909d08135";
const BTRFS_LABEL: &str = "USBBTRFS";
const DEVICE: &str = "stick";
const LABEL: [u8; 11] = *b"USBSTICK   ";
const HELLO: &[u8] = b"hello from the host\n";
const WRITTEN: &[u8] = b"written on Mikros\n";

/// Writes the USB disk, `usb.img` in `dir`, in place of the scratch disk.
fn write_stick(dir: &Path) -> Result<()> {
    let mut image = Cursor::new(vec![0u8; (SECTORS * gpt::SECTOR) as usize]);
    let partitions = [
        gpt::Partition {
            type_guid: uuid::Uuid::parse_str(BASIC_DATA)?,
            guid: uuid::Uuid::parse_str(PARTITION_GUID)?,
            name: "stick",
            first_lba: FIRST_LBA,
            last_lba: LAST_LBA,
        },
        gpt::Partition {
            type_guid: uuid::Uuid::parse_str(MIKROS_ROOT)?,
            guid: uuid::Uuid::parse_str(BTRFS_GUID)?,
            name: "btrfs",
            first_lba: BTRFS_FIRST_LBA,
            last_lba: BTRFS_FIRST_LBA + BTRFS_SECTORS - 1,
        },
    ];
    let disk_guid = uuid::Uuid::parse_str(DISK_GUID)?;
    gpt::write(&mut image, SECTORS, disk_guid, &partitions)?;
    let size = (LAST_LBA - FIRST_LBA + 1) * gpt::SECTOR;
    let volume = esp::build_volume(size, fatfs::FatType::Fat12, LABEL, &[("HELLO.TXT", HELLO)])?;
    let mut bytes = image.into_inner();
    let start = (FIRST_LBA * gpt::SECTOR) as usize;
    bytes[start..start + volume.len()].copy_from_slice(&volume);
    let btrfs = btrfs_volume(dir)?;
    let start = (BTRFS_FIRST_LBA * gpt::SECTOR) as usize;
    bytes[start..start + btrfs.len()].copy_from_slice(&btrfs);
    let path = dir.join("usb.img");
    fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// A btrfs of the btrfs partition's size labelled `USBBTRFS` with
/// `HELLO.TXT` on it, made by mkfs.btrfs in `dir`.
fn btrfs_volume(dir: &Path) -> Result<Vec<u8>> {
    let tree = dir.join("usb-btrfs-tree");
    let _ = fs::remove_dir_all(&tree);
    fs::create_dir_all(&tree)?;
    fs::write(tree.join("HELLO.TXT"), HELLO)?;
    let image = dir.join("usb-btrfs.img");
    fs::File::create(&image)?.set_len(BTRFS_SECTORS * gpt::SECTOR)?;
    let status = std::process::Command::new("mkfs.btrfs")
        .args(["-q", "-f", "-L", BTRFS_LABEL, "--rootdir"])
        .arg(&tree)
        .arg(&image)
        .status()
        .context("failed to run mkfs.btrfs")?;
    if !status.success() {
        bail!("mkfs.btrfs failed with {status}");
    }
    let bytes = fs::read(&image)?;
    fs::remove_dir_all(&tree)?;
    fs::remove_file(&image)?;
    Ok(bytes)
}

/// The contents of `name` in the root of the FAT volume on the USB disk
/// in `dir`.
fn read_stick(dir: &Path, name: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(dir.join("usb.img"))?;
    let start = (FIRST_LBA * gpt::SECTOR) as usize;
    let end = ((LAST_LBA + 1) * gpt::SECTOR) as usize;
    let mut volume = Cursor::new(bytes[start..end].to_vec());
    let fs = fatfs::FileSystem::new(&mut volume, fatfs::FsOptions::new())?;
    let mut data = Vec::new();
    fs.root_dir().open_file(name)?.read_to_end(&mut data)?;
    Ok(data)
}

/// Boots the USB volumes run of the components `built` of the build kind
/// `kind` in `dir`, and checks what the probe logs and what it wrote.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    image::create_with(kind, built, &dir.join("disk.img"), "test=usb-volumes", &[])?;
    let log = dir.join("serial.log");
    let socket = scenario::monitor_socket(dir);
    let mut qemu = scenario::start_with(machine, dir, false, &write_stick)?;
    let deadline = Instant::now() + timeout;
    let ended = (|| -> Result<()> {
        scenario::wait_for_line(&log, READY, &mut qemu, deadline)?;
        scenario::monitor(&socket, &format!("device_del {DEVICE}"))?;
        scenario::wait_for_line(&log, GONE, &mut qemu, deadline)?;
        Ok(())
    })();
    let _ = qemu.kill();
    let _ = qemu.wait();
    let serial = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
    if serial.contains(PANIC_LINE) {
        bail!("the kernel panicked; serial output in {}", log.display());
    }
    if let Some(line) = serial.lines().find(|l| l.contains(FAILED)) {
        bail!("{line}; serial output in {}", log.display());
    }
    ended.with_context(|| {
        format!(
            "the USB volumes run failed; serial output in {}",
            log.display()
        )
    })?;
    let written = read_stick(dir, "WRITTEN.TXT").context("reading what the probe wrote")?;
    if written != WRITTEN {
        bail!("WRITTEN.TXT on the USB FAT volume holds {written:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_labelled_fat_volume_on_the_stick() {
        let dir = std::env::temp_dir().join(format!("volumes-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        write_stick(&dir).unwrap();
        assert_eq!(read_stick(&dir, "HELLO.TXT").unwrap(), HELLO);
        let bytes = fs::read(dir.join("usb.img")).unwrap();
        assert_eq!(bytes.len() as u64, SECTORS * gpt::SECTOR);
        fs::remove_dir_all(&dir).unwrap();
    }
}
