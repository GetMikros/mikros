//! The run of the test image in which the volumes of USB disks come and
//! go in `/Volumes`, each with `HELLO.TXT` on it:
//! - The first disk holds a GPT with a basic data partition with a FAT
//!   file system labelled `USBSTICK`, a Mikros root type partition with a
//!   btrfs labelled `USBBTRFS`, and another basic data partition with an
//!   exFAT file system labelled `GPTEXFAT`.
//! - The second holds an MBR with a FAT12 primary partition labelled
//!   `MBRFAT`, and an extended partition whose logical partition, of the
//!   Linux type `0x83`, holds a btrfs labelled `MBRBTRFS`.
//! - The third holds a FAT labelled `WHOLEFAT` on the whole disk, without
//!   a partition table.
//!
//! The test init serves `tests/volumes.img` with `test=usb-volumes`,
//! whose volumes probe reads and writes every volume through `/Volumes`.
//! The runner then unplugs the disks through QEMU's monitor and waits for
//! the probe to see the volumes leave. Afterwards each FAT and exFAT
//! volume must hold what the probe wrote, and fsck.exfat must find the
//! exFAT volume clean.

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
/// The USB disk: the blocks of its FAT, btrfs and exFAT partitions, its
/// size in sectors with room for the backup GPT, its GUIDs, the partition
/// types, and the QEMU device ID it is unplugged by.
const FIRST_LBA: u64 = 34;
const LAST_LBA: u64 = 2047;
const BTRFS_FIRST_LBA: u64 = 2048;
const BTRFS_SECTORS: u64 = 128 << 11;
const EXFAT_FIRST_LBA: u64 = BTRFS_FIRST_LBA + BTRFS_SECTORS;
const EXFAT_SECTORS: u64 = 4 << 11;
const SECTORS: u64 = EXFAT_FIRST_LBA + EXFAT_SECTORS + 34;
const DISK_GUID: &str = "55534221-0000-4000-8000-0000000000d4";
const PARTITION_GUID: &str = "55534221-0000-4000-8000-000000000004";
const BTRFS_GUID: &str = "55534221-0000-4000-8000-000000000005";
const EXFAT_GUID: &str = "55534221-0000-4000-8000-000000000006";
const BASIC_DATA: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";
const MIKROS_ROOT: &str = "fa730af6-8ba3-42d9-b155-466909d08135";
const BTRFS_LABEL: &str = "USBBTRFS";
const EXFAT_LABEL: &str = "GPTEXFAT";
const DEVICE: &str = "stick";
const LABEL: [u8; 11] = *b"USBSTICK   ";
/// The second disk: its QEMU device ID, its disk signature, the blocks of
/// its FAT partition, where its extended partition starts, where its
/// logical partition starts after the extended boot record, and the
/// labels.
const MBR_DEVICE: &str = "stick2";
const MBR_FILE: &str = "usb-mbr.img";
const MBR_SIGNATURE: u32 = 0x4d42_5221;
const MBR_FAT_FIRST: u64 = 2048;
const MBR_FAT_SECTORS: u64 = 2048;
const MBR_EXTENDED_FIRST: u64 = 4096;
const MBR_LOGICAL_OFFSET: u64 = 2048;
const MBR_SECTORS: u64 = MBR_EXTENDED_FIRST + MBR_LOGICAL_OFFSET + BTRFS_SECTORS;
const MBR_LABEL: [u8; 11] = *b"MBRFAT     ";
const MBR_BTRFS_LABEL: &str = "MBRBTRFS";
/// The third disk: its QEMU device ID, its size in sectors and its
/// label.
const WHOLE_DEVICE: &str = "stick3";
const WHOLE_FILE: &str = "usb-whole.img";
const WHOLE_SECTORS: u64 = 2048;
const WHOLE_LABEL: [u8; 11] = *b"WHOLEFAT   ";
const HELLO: &[u8] = b"hello from the host\n";
const WRITTEN: &[u8] = b"written on Mikros\n";

/// Writes the USB disks in `dir`: the first, `usb.img`, in place of the
/// scratch disk, and the other two.
fn write_sticks(dir: &Path) -> Result<()> {
    write_stick(dir)?;
    write_mbr_stick(dir)?;
    let whole = esp::build_volume(
        WHOLE_SECTORS * gpt::SECTOR,
        fatfs::FatType::Fat12,
        WHOLE_LABEL,
        &[("HELLO.TXT", HELLO)],
    )?;
    let path = dir.join(WHOLE_FILE);
    fs::write(&path, whole).with_context(|| format!("writing {}", path.display()))
}

/// Writes the second USB disk, with its MBR and extended boot record.
fn write_mbr_stick(dir: &Path) -> Result<()> {
    let mut bytes = vec![0u8; (MBR_SECTORS * gpt::SECTOR) as usize];
    // Sets entry `i` of the boot record at `record`, and its signature.
    let entry = |bytes: &mut [u8], record: u64, i: usize, kind: u8, start: u64, count: u64| {
        let base = (record * gpt::SECTOR) as usize;
        let at = base + 446 + 16 * i;
        bytes[at + 4] = kind;
        bytes[at + 8..at + 12].copy_from_slice(&(start as u32).to_le_bytes());
        bytes[at + 12..at + 16].copy_from_slice(&(count as u32).to_le_bytes());
        bytes[base + 510..base + 512].copy_from_slice(&[0x55, 0xAA]);
    };
    bytes[440..444].copy_from_slice(&MBR_SIGNATURE.to_le_bytes());
    entry(&mut bytes, 0, 0, 0x01, MBR_FAT_FIRST, MBR_FAT_SECTORS);
    let extended = MBR_SECTORS - MBR_EXTENDED_FIRST;
    entry(&mut bytes, 0, 1, 0x0f, MBR_EXTENDED_FIRST, extended);
    // The logical partition starts after its record, by the record's LBA.
    entry(
        &mut bytes,
        MBR_EXTENDED_FIRST,
        0,
        0x83,
        MBR_LOGICAL_OFFSET,
        BTRFS_SECTORS,
    );
    let size = MBR_FAT_SECTORS * gpt::SECTOR;
    let fat = esp::build_volume(
        size,
        fatfs::FatType::Fat12,
        MBR_LABEL,
        &[("HELLO.TXT", HELLO)],
    )?;
    let start = (MBR_FAT_FIRST * gpt::SECTOR) as usize;
    bytes[start..start + fat.len()].copy_from_slice(&fat);
    let btrfs = btrfs_volume(dir, MBR_BTRFS_LABEL)?;
    let start = ((MBR_EXTENDED_FIRST + MBR_LOGICAL_OFFSET) * gpt::SECTOR) as usize;
    bytes[start..start + btrfs.len()].copy_from_slice(&btrfs);
    let path = dir.join(MBR_FILE);
    fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// QEMU's arguments for the second and third disks in `dir`, on ports 3
/// and 4 of the xHCI controller's bus.
fn more_sticks(dir: &Path) -> Vec<String> {
    let mut args = Vec::new();
    for (device, file, port) in [(MBR_DEVICE, MBR_FILE, 3), (WHOLE_DEVICE, WHOLE_FILE, 4)] {
        let path = dir.join(file);
        args.extend([
            "-drive".into(),
            format!(
                "if=none,id={device}-drive,format=raw,file={}",
                path.display()
            ),
            "-device".into(),
            format!(
                "usb-storage,id={device},bus=usb.0,port={port},drive={device}-drive,serial=mikros"
            ),
        ]);
    }
    args
}

/// Writes the first USB disk, `usb.img` in `dir`.
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
        gpt::Partition {
            type_guid: uuid::Uuid::parse_str(BASIC_DATA)?,
            guid: uuid::Uuid::parse_str(EXFAT_GUID)?,
            name: "exfat",
            first_lba: EXFAT_FIRST_LBA,
            last_lba: EXFAT_FIRST_LBA + EXFAT_SECTORS - 1,
        },
    ];
    let disk_guid = uuid::Uuid::parse_str(DISK_GUID)?;
    gpt::write(&mut image, SECTORS, disk_guid, &partitions)?;
    let size = (LAST_LBA - FIRST_LBA + 1) * gpt::SECTOR;
    let volume = esp::build_volume(size, fatfs::FatType::Fat12, LABEL, &[("HELLO.TXT", HELLO)])?;
    let mut bytes = image.into_inner();
    let start = (FIRST_LBA * gpt::SECTOR) as usize;
    bytes[start..start + volume.len()].copy_from_slice(&volume);
    let btrfs = btrfs_volume(dir, BTRFS_LABEL)?;
    let start = (BTRFS_FIRST_LBA * gpt::SECTOR) as usize;
    bytes[start..start + btrfs.len()].copy_from_slice(&btrfs);
    let exfat = exfat_volume(dir, EXFAT_LABEL)?;
    let start = (EXFAT_FIRST_LBA * gpt::SECTOR) as usize;
    bytes[start..start + exfat.len()].copy_from_slice(&exfat);
    let path = dir.join("usb.img");
    fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// A btrfs of `BTRFS_SECTORS` labelled `label` with `HELLO.TXT` on it,
/// made by mkfs.btrfs in `dir`.
fn btrfs_volume(dir: &Path, label: &str) -> Result<Vec<u8>> {
    let tree = dir.join("usb-btrfs-tree");
    let _ = fs::remove_dir_all(&tree);
    fs::create_dir_all(&tree)?;
    fs::write(tree.join("HELLO.TXT"), HELLO)?;
    let image = dir.join("usb-btrfs.img");
    fs::File::create(&image)?.set_len(BTRFS_SECTORS * gpt::SECTOR)?;
    let status = std::process::Command::new("mkfs.btrfs")
        .args(["-q", "-f", "-L", label, "--rootdir"])
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

/// A volume in memory, for mikros-exfat.
struct Memory(Vec<u8>);

impl mikros_exfat::Device for Memory {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), mikros_exfat::Error> {
        let at = offset as usize;
        let bytes = self.0.get(at..at + buf.len());
        buf.copy_from_slice(bytes.ok_or(mikros_exfat::Error::Io)?);
        Ok(())
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), mikros_exfat::Error> {
        let at = offset as usize;
        let bytes = self.0.get_mut(at..at + data.len());
        bytes.ok_or(mikros_exfat::Error::Io)?.copy_from_slice(data);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), mikros_exfat::Error> {
        Ok(())
    }
}

/// An exFAT volume of `EXFAT_SECTORS` labelled `label`, which mkfs.exfat
/// formats in `dir`, with `HELLO.TXT`, which mikros-exfat puts on it,
/// modified on 2024-01-01 at midnight UTC.
fn exfat_volume(dir: &Path, label: &str) -> Result<Vec<u8>> {
    let image = dir.join("usb-exfat.img");
    fs::File::create(&image)?.set_len(EXFAT_SECTORS * gpt::SECTOR)?;
    let output = std::process::Command::new("mkfs.exfat")
        .args(["-L", label])
        .arg(&image)
        .output()
        .context("failed to run mkfs.exfat")?;
    if !output.status.success() {
        bail!("mkfs.exfat failed with {}", output.status);
    }
    let bytes = fs::read(&image)?;
    fs::remove_file(&image)?;
    let size = bytes.len() as u64;
    let failed = |e| anyhow::anyhow!("putting HELLO.TXT on the exFAT volume: {e:?}");
    let mut volume = mikros_exfat::Volume::mount(Memory(bytes), size, true).map_err(failed)?;
    let stamp = mikros_exfat::Stamp {
        timestamp: 44 << 25 | 1 << 21 | 1 << 16,
        increment: 0,
        utc_offset: 0x80,
    };
    let root = volume.root();
    let mut hello = volume
        .create(root, "HELLO.TXT", false, stamp)
        .map_err(failed)?;
    volume.write(&mut hello, 0, HELLO, stamp).map_err(failed)?;
    volume.unmount().map_err(failed)?;
    Ok(volume.into_device().0)
}

/// The contents of `name` in the root of the exFAT volume of
/// `EXFAT_SECTORS` from `EXFAT_FIRST_LBA` on `usb.img` in `dir`, once
/// fsck.exfat has found the volume clean.
fn read_exfat(dir: &Path, name: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(dir.join("usb.img"))?;
    let start = (EXFAT_FIRST_LBA * gpt::SECTOR) as usize;
    let end = ((EXFAT_FIRST_LBA + EXFAT_SECTORS) * gpt::SECTOR) as usize;
    let image = dir.join("usb-exfat.img");
    fs::write(&image, &bytes[start..end])?;
    let output = std::process::Command::new("fsck.exfat")
        .arg("-n")
        .arg(&image)
        .output()
        .context("failed to run fsck.exfat")?;
    fs::remove_file(&image)?;
    if !output.status.success() {
        bail!(
            "fsck.exfat found the exFAT volume damaged: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    let failed = |e| anyhow::anyhow!("reading the exFAT volume: {e:?}");
    let volume = Memory(bytes[start..end].to_vec());
    let size = (end - start) as u64;
    let mut volume = mikros_exfat::Volume::mount(volume, size, false).map_err(failed)?;
    let root = volume.root();
    let mut node = volume.find(root, name).map_err(failed)?;
    let mut data = vec![0u8; node.size() as usize];
    volume.read(&mut node, 0, &mut data).map_err(failed)?;
    Ok(data)
}

/// The contents of `name` in the root of the FAT volume of `sectors`
/// from `first` on the disk `file` in `dir`.
fn read_fat(dir: &Path, file: &str, first: u64, sectors: u64, name: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(dir.join(file))?;
    let start = (first * gpt::SECTOR) as usize;
    let end = ((first + sectors) * gpt::SECTOR) as usize;
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
    let extra = more_sticks(dir);
    let mut qemu = scenario::start_with(machine, dir, false, &write_sticks, &extra)?;
    let deadline = Instant::now() + timeout;
    let ended = (|| -> Result<()> {
        scenario::wait_for_line(&log, READY, &mut qemu, deadline)?;
        for device in [DEVICE, MBR_DEVICE, WHOLE_DEVICE] {
            scenario::monitor(&socket, &format!("device_del {device}"))?;
        }
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
    let fats = [
        ("usb.img", FIRST_LBA, LAST_LBA - FIRST_LBA + 1),
        (MBR_FILE, MBR_FAT_FIRST, MBR_FAT_SECTORS),
        (WHOLE_FILE, 0, WHOLE_SECTORS),
    ];
    for (file, first, sectors) in fats {
        let written = read_fat(dir, file, first, sectors, "WRITTEN.TXT")
            .with_context(|| format!("reading what the probe wrote on {file}"))?;
        if written != WRITTEN {
            bail!("WRITTEN.TXT on the FAT volume of {file} holds {written:?}");
        }
    }
    let written = read_exfat(dir, "WRITTEN.TXT")?;
    if written != WRITTEN {
        bail!("WRITTEN.TXT on the exFAT volume holds {written:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_labelled_fat_volumes_on_the_sticks() {
        let dir = std::env::temp_dir().join(format!("volumes-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        write_sticks(&dir).unwrap();
        let sectors = LAST_LBA - FIRST_LBA + 1;
        assert_eq!(
            read_fat(&dir, "usb.img", FIRST_LBA, sectors, "HELLO.TXT").unwrap(),
            HELLO
        );
        let bytes = fs::read(dir.join("usb.img")).unwrap();
        assert_eq!(bytes.len() as u64, SECTORS * gpt::SECTOR);
        let mbr = read_fat(&dir, MBR_FILE, MBR_FAT_FIRST, MBR_FAT_SECTORS, "HELLO.TXT");
        assert_eq!(mbr.unwrap(), HELLO);
        let bytes = fs::read(dir.join(MBR_FILE)).unwrap();
        assert_eq!(bytes[510..512], [0x55, 0xAA]);
        let ebr = (MBR_EXTENDED_FIRST * gpt::SECTOR) as usize;
        assert_eq!(bytes[ebr + 446 + 4], 0x83);
        let whole = read_fat(&dir, WHOLE_FILE, 0, WHOLE_SECTORS, "HELLO.TXT");
        assert_eq!(whole.unwrap(), HELLO);
        assert_eq!(read_exfat(&dir, "HELLO.TXT").unwrap(), HELLO);
        fs::remove_dir_all(&dir).unwrap();
    }
}
