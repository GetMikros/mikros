//! The runs of the test image whose test init scenario stops QEMU when it
//! passed or failed:
//! - the root disk runs, behind an NVMe controller and behind the AHCI
//!   controller: the test init serves `tests/root.img` with
//!   `test=root-disk`, until the process manager has switched to the root
//!   and serves `/system/boot/efi`;
//! - the fresh GUIDs run: the root disk run behind an NVMe controller with
//!   `renew_guids` in `boot.conf`, after which the disk has new partition
//!   GUIDs and `boot.conf` asks for none;
//! - the root clone run: as the root disk run behind an NVMe controller,
//!   with `test=root-clone` and a copy of the boot disk as the scratch
//!   disk behind the AHCI controller, whose root and EFI system
//!   partitions the device manager must log and ignore (MIKSPEC-0045.R21,
//!   MIKSPEC-0025.R71);
//! - the bus restart runs, behind a virtio-blk, an NVMe and the AHCI
//!   controller: the test init serves `tests/bus.img` with
//!   `test=bus-restart`, whose restart probe kills the PCI bus driver
//!   below the mounted root, and then the drivers of the disk
//!   controllers, the root's among them, mid-write, until the probe finds
//!   the other drivers still running and the writes on the root intact;
//! - the ACPI restart run: the test init serves `tests/acpi-restart.img`
//!   with `test=acpi-restart`, whose restart probe kills the ACPI server
//!   below the real-time clock's driver, until the probe finds the other
//!   drivers and the clock still working.
//!
//! The test init stops QEMU through `isa-debug-exit` once the run passed
//! or failed.

use crate::build::{self, Kind};
use crate::qemu::{Disk, MachineArgs};
use crate::{image, scenario};
use anyhow::{Result, bail};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// A kind of run: its name, the test init's scenario, the line the
/// test init logs once it passed, other lines it must log, the
/// controllers the boot disk is put on, with the name of each run, and
/// whether the scratch disk behind the AHCI controller holds a copy of
/// the boot disk.
struct Runs {
    name: &'static str,
    scenario: &'static str,
    passed: &'static str,
    also: &'static [&'static str],
    disks: &'static [(&'static str, Disk)],
    clone: bool,
}

const ROOT_DISK: Runs = Runs {
    name: "root disk",
    scenario: "test=root-disk",
    passed: "SCENARIO root-disk PASSED",
    also: &[],
    disks: &[("nvme", Disk::Nvme), ("ahci", Disk::Ahci)],
    clone: false,
};

const ROOT_CLONE: Runs = Runs {
    name: "root clone",
    scenario: "test=root-clone",
    passed: "SCENARIO root-clone PASSED",
    also: &[],
    disks: &[("nvme", Disk::Nvme)],
    clone: true,
};

const ACPI_RESTART: Runs = Runs {
    name: "ACPI restart",
    scenario: "test=acpi-restart",
    passed: "SCENARIO acpi-restart PASSED",
    also: &[],
    disks: &[("virtio", Disk::Virtio)],
    clone: false,
};

const BUS_RESTART: Runs = Runs {
    name: "bus restart",
    scenario: "test=bus-restart",
    passed: "SCENARIO bus-restart PASSED",
    // The restarted PCI bus driver takes the BAR sizes of the functions
    // whose drivers run on from the device manager, without sizing them.
    also: &["pci-bus: took the BAR sizes of "],
    disks: &[
        ("virtio", Disk::Virtio),
        ("nvme", Disk::Nvme),
        ("ahci", Disk::Ahci),
    ],
    clone: false,
};

/// Why a run that logs `passed` once it passes, and each line of `also`,
/// did not pass, if it did not, given QEMU's exit code and the serial
/// output.
fn verdict(code: Option<i32>, serial: &str, passed: &str, also: &[&str]) -> Result<(), String> {
    crate::test::verdict(code, serial)?;
    if let Some(line) = core::iter::once(&passed)
        .chain(also)
        .find(|l| !serial.contains(*l))
    {
        return Err(format!("{line:?} is missing"));
    }
    Ok(())
}

/// Boots the root disk runs of the components `built` of the build kind
/// `kind`, each in a directory of `dir` named after its controller, and
/// then the root clone run in `clone` of `dir`.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    run_all(&ROOT_DISK, machine, kind, built, dir, timeout)?;
    run_all(
        &ROOT_CLONE,
        machine,
        kind,
        built,
        &dir.join("clone"),
        timeout,
    )
}

/// Boots the bus restart runs, as `run` does the root disk runs.
pub fn run_bus_restart(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    run_all(&BUS_RESTART, machine, kind, built, dir, timeout)
}

/// Boots the ACPI restart run, as `run` does the root disk runs.
pub fn run_acpi_restart(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    run_all(&ACPI_RESTART, machine, kind, built, dir, timeout)
}

fn run_all(
    runs: &Runs,
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    for &(name, disk) in runs.disks {
        let dir = dir.join(name);
        fs::create_dir_all(&dir)?;
        image::create_with(kind, built, &dir.join("disk.img"), runs.scenario, &[])?;
        let mut machine = machine.clone();
        machine.disk = disk;
        boot(runs, name, &machine, &dir, timeout)?;
        println!("{} run {name} passed", runs.name);
    }
    Ok(())
}

/// Boots `machine` from the disk image in `dir` once for the run `name`
/// of `runs`, and fails unless it passed in time.
fn boot(
    runs: &Runs,
    name: &str,
    machine: &MachineArgs,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    let what = runs.name;
    let log = dir.join("serial.log");
    // The AHCI scratch disk's image, written with the scratch disks,
    // becomes a copy of the boot disk. Its driver is in the boot image,
    // unlike xhci, which the test image's root does not have.
    let clone = |dir: &Path| -> Result<()> {
        fs::copy(dir.join("disk.img"), dir.join("ahci.img"))?;
        Ok(())
    };
    let mut qemu = match runs.clone {
        true => scenario::start_with(machine, dir, false, &clone, &[], false)?,
        false => scenario::start(machine, dir, false)?,
    };
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = qemu.try_wait()? {
            break Some(status);
        }
        if Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = qemu.kill();
    let _ = qemu.wait();
    let Some(status) = status else {
        bail!(
            "the {name} {what} run timed out; serial output in {}",
            log.display()
        );
    };
    let serial = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
    if let Err(why) = verdict(status.code(), &serial, runs.passed, runs.also) {
        bail!(
            "the {name} {what} run failed: {why}; serial output in {}",
            log.display()
        );
    }
    Ok(())
}

const FRESH_GUIDS: Runs = Runs {
    name: "fresh GUIDs",
    scenario: "test=root-disk",
    passed: "SCENARIO root-disk PASSED",
    also: &[],
    disks: &[("nvme", Disk::Nvme)],
    clone: false,
};

/// Boots the fresh GUIDs run in `dir`: the root disk run's image with
/// `renew_guids` in its `boot.conf`, from an NVMe controller. The boot
/// must pass with new GUIDs for the disk and each partition, the same in
/// both tables, and leave a `boot.conf` without `renew_guids` and the old
/// `root=PARTUUID=` and `esp=PARTUUID=` options (MIKSPEC-0004.R41). It is
/// booted once: the test init stops QEMU while the EFI system partition
/// is mounted, which leaves it not shut down cleanly for a second boot.
pub fn run_fresh_guids(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    let dir = dir.join("nvme");
    fs::create_dir_all(&dir)?;
    let disk = dir.join("disk.img");
    let extra = format!("{} {}", FRESH_GUIDS.scenario, image::RENEW_GUIDS);
    image::create_with(kind, built, &disk, &extra, &[])?;
    let mut machine = machine.clone();
    machine.disk = Disk::Nvme;
    let before = Table::read(&disk)?;
    boot(&FRESH_GUIDS, "nvme", &machine, &dir, timeout)?;
    let after = Table::read(&disk)?;
    let all_new = after.disk != before.disk
        && after.partitions.len() == before.partitions.len()
        && after
            .partitions
            .iter()
            .all(|(_, g, _)| before.partitions.iter().all(|(_, old, _)| old != g));
    if !all_new {
        bail!("the boot did not renew every GUID: {before:?} became {after:?}");
    }
    let conf = after.boot_conf(&disk)?;
    let left = conf.split_ascii_whitespace().find(|w| {
        *w == image::RENEW_GUIDS
            || w.starts_with("root=PARTUUID=")
            || w.starts_with("esp=PARTUUID=")
    });
    if let Some(word) = left {
        bail!("boot.conf still holds {word} after the boot: {conf:?}");
    }
    Ok(())
}

/// A used entry of a partition table: its type GUID, partition GUID,
/// and first and last LBA, as stored.
type Entry = ([u8; 16], [u8; 16], (u64, u64));

/// A disk image's partition table: the disk GUID and the used entries.
#[derive(Debug, PartialEq, Eq)]
struct Table {
    disk: [u8; 16],
    partitions: Vec<Entry>,
}

impl Table {
    /// The table of the disk image at `path`, the same in its primary
    /// and its backup copy.
    fn read(path: &Path) -> Result<Table> {
        use std::os::unix::fs::FileExt;
        let file = fs::File::open(path)?;
        let sector = image::gpt::SECTOR;
        let last = file.metadata()?.len() / sector - 1;
        let read = |at: u64, len: usize| -> Result<Vec<u8>> {
            let mut bytes = vec![0u8; len];
            file.read_exact_at(&mut bytes, at)?;
            Ok(bytes)
        };
        let u64_at = |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().expect("8"));
        let u32_at = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().expect("4"));
        let guid = |b: &[u8]| -> [u8; 16] { b[..16].try_into().expect("16 bytes") };
        let table = |lba: u64| -> Result<Table> {
            let header = read(lba * sector, sector as usize)?;
            let (count, size) = (u32_at(&header, 80) as usize, u32_at(&header, 84) as usize);
            let entries = read(u64_at(&header, 72) * sector, count * size)?;
            let partitions = entries
                .chunks_exact(size)
                .filter(|e| e[..16] != [0; 16])
                .map(|e| (guid(e), guid(&e[16..]), (u64_at(e, 32), u64_at(e, 40))))
                .collect();
            Ok(Table {
                disk: guid(&header[56..]),
                partitions,
            })
        };
        let (primary, backup) = (table(1)?, table(last)?);
        if primary != backup {
            bail!(
                "the tables of {} differ: {primary:?}, {backup:?}",
                path.display()
            );
        }
        Ok(primary)
    }

    /// The text of `EFI/Mikros/boot.conf` on the EFI system partition of
    /// the disk image at `path`.
    fn boot_conf(&self, path: &Path) -> Result<String> {
        use std::io::Read;
        use std::os::unix::fs::FileExt;
        let esp = image::ESP_TYPE.to_bytes_le();
        let (_, _, (first, last)) = self
            .partitions
            .iter()
            .find(|(kind, _, _)| *kind == esp)
            .ok_or_else(|| anyhow::anyhow!("{} has no EFI system partition", path.display()))?;
        let sector = image::gpt::SECTOR;
        let mut bytes = vec![0u8; ((last - first + 1) * sector) as usize];
        fs::File::open(path)?.read_exact_at(&mut bytes, first * sector)?;
        let volume = fatfs::FileSystem::new(std::io::Cursor::new(bytes), fatfs::FsOptions::new())?;
        let mut text = String::new();
        volume
            .root_dir()
            .open_file("EFI/Mikros/boot.conf")?
            .read_to_string(&mut text)?;
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_the_passed_code_and_line() {
        let passed = crate::test::PASSED;
        for runs in [&ROOT_DISK, &BUS_RESTART, &ACPI_RESTART] {
            let line = runs.passed;
            let all = core::iter::once(line)
                .chain(runs.also.iter().copied())
                .collect::<Vec<_>>()
                .join("\n");
            let also = runs.also;
            assert_eq!(verdict(Some(passed), &all, line, also), Ok(()));
            assert!(verdict(Some(passed), "SCENARIO root-disk FAILED", line, also).is_err());
            assert!(verdict(Some(passed + 2), &all, line, also).is_err());
            assert!(verdict(None, &all, line, also).is_err());
            let panicked = format!("{all}\nKERNEL PANIC: page fault");
            assert!(verdict(Some(passed), &panicked, line, also).is_err());
            if !also.is_empty() {
                assert!(verdict(Some(passed), line, line, also).is_err());
            }
        }
    }
}
