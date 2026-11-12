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
/// test init logs once it passed, other lines it must log, how many
/// times the PCI bus driver starts, and the controllers the boot disk is
/// put on, with the name of each run.
struct Runs {
    name: &'static str,
    scenario: &'static str,
    passed: &'static str,
    also: &'static [&'static str],
    bus_starts: usize,
    disks: &'static [(&'static str, Disk)],
}

const ROOT_DISK: Runs = Runs {
    name: "root disk",
    scenario: "test=root-disk",
    passed: "SCENARIO root-disk PASSED",
    also: &[],
    bus_starts: 1,
    disks: &[("nvme", Disk::Nvme), ("ahci", Disk::Ahci)],
};

const ACPI_RESTART: Runs = Runs {
    name: "ACPI restart",
    scenario: "test=acpi-restart",
    passed: "SCENARIO acpi-restart PASSED",
    also: &[],
    // Its image has no PCI bus driver.
    bus_starts: 0,
    disks: &[("virtio", Disk::Virtio)],
};

const BUS_RESTART: Runs = Runs {
    name: "bus restart",
    scenario: "test=bus-restart",
    passed: "SCENARIO bus-restart PASSED",
    // The restarted PCI bus driver takes the BAR sizes of the functions
    // whose drivers run on from the device manager, without sizing them.
    also: &["pci-bus: took the BAR sizes of "],
    bus_starts: 2,
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

/// What the PCI bus driver logs for each display function that it
/// leaves unsized when it starts, before the function's name.
const LEFT_UNSIZED: &str = "pci-bus: left the display function ";

/// Why the PCI bus driver did not leave QEMU's display function, which
/// shows the boot framebuffer, unsized each of the `starts` times it
/// started, or why such a function was added with BAR sizes, if so,
/// given the serial output. QEMU's display keeps working when it is
/// sized, so only this shows that the bus driver left it alone.
fn display_unsized(serial: &str, starts: usize) -> Result<(), String> {
    let names: Vec<&str> = serial
        .lines()
        .filter_map(|line| line.split_once(LEFT_UNSIZED)?.1.split_whitespace().next())
        .collect();
    if names.len() != starts {
        return Err(format!(
            "{LEFT_UNSIZED:?} was logged {} times, not {starts}",
            names.len()
        ));
    }
    for name in &names {
        let added = format!(", {name}, as ");
        let sized = serial.lines().find(|line| {
            line.contains("device-manager: added device ")
                && line.contains(&added)
                && (0..6).any(|i| line.contains(&format!(", bar{i} ")))
        });
        if let Some(line) = sized {
            return Err(format!("{name} was added with BAR sizes: {line}"));
        }
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
        let checked = verdict(status.code(), &serial, runs.passed, runs.also)
            .and_then(|()| display_unsized(&serial, runs.bus_starts));
        if let Err(why) = checked {
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

    #[test]
    fn needs_the_display_function_left_unsized_at_every_start() {
        let left = "[0.85] I p858: pci-bus: left the display function pci-0000:00:03.0 \
                    unsized until a driver asks for a BAR";
        let added = "[0.86] I p0: device-manager: added device 5, pci-0000:00:03.0, \
                     as pci:v1234d1111sv1af4sd1100r02, under 2, with 0 resources";
        let sized = format!("{added}, bar0 0x1000000, bar2 0x1000");
        let other = "[0.86] I p0: device-manager: added device 6, pci-0000:00:04.0, \
                     as pci:v1af4d1042sv1af4sd1100r01, under 2, with 0 resources, bar1 0x1000";
        let once = [left, added, other].join("\n");
        assert_eq!(display_unsized(&once, 1), Ok(()));
        let twice = [left, added, other, left].join("\n");
        assert_eq!(display_unsized(&twice, 2), Ok(()));
        // Missing at a start, or logged more often than the bus starts.
        assert!(display_unsized(&once, 2).is_err());
        assert!(display_unsized(&twice, 1).is_err());
        assert!(display_unsized(other, 1).is_err());
        // Added with BAR sizes.
        let with_bars = [left, &sized, other].join("\n");
        assert!(display_unsized(&with_bars, 1).is_err());
    }
}
