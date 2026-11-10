//! The runs of the test image whose test init scenario stops QEMU when it
//! passed or failed:
//! - the root disk runs, behind an NVMe controller and behind the AHCI
//!   controller: the test init serves `tests/root.img` with
//!   `test=root-disk`, until the process manager has switched to the root
//!   and serves `/boot/efi`;
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
/// test init logs once it passed, other lines it must log, and the
/// controllers the boot disk is put on, with the name of each run.
struct Runs {
    name: &'static str,
    scenario: &'static str,
    passed: &'static str,
    also: &'static [&'static str],
    disks: &'static [(&'static str, Disk)],
}

const ROOT_DISK: Runs = Runs {
    name: "root disk",
    scenario: "test=root-disk",
    passed: "SCENARIO root-disk PASSED",
    also: &[],
    disks: &[("nvme", Disk::Nvme), ("ahci", Disk::Ahci)],
};

const ACPI_RESTART: Runs = Runs {
    name: "ACPI restart",
    scenario: "test=acpi-restart",
    passed: "SCENARIO acpi-restart PASSED",
    also: &[],
    disks: &[("virtio", Disk::Virtio)],
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
/// `kind`, each in a directory of `dir` named after its controller.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    run_all(&ROOT_DISK, machine, kind, built, dir, timeout)
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
    let what = runs.name;
    for &(name, disk) in runs.disks {
        let dir = dir.join(name);
        fs::create_dir_all(&dir)?;
        image::create_with(kind, built, &dir.join("disk.img"), runs.scenario, &[])?;
        let mut machine = machine.clone();
        machine.disk = disk;
        let log = dir.join("serial.log");
        let mut qemu = scenario::start(&machine, &dir, false)?;
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
        println!("{what} run {name} passed");
    }
    Ok(())
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
