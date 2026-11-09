//! The runs of the test image that bring up the root file system from a
//! boot disk behind an NVMe controller and behind the AHCI controller.
//! The test init serves `tests/root.img` with `test=root-disk`, and once
//! the process manager has switched to the root and serves `/boot/efi`,
//! or once that failed, it stops QEMU through `isa-debug-exit`.

use crate::build::{self, Kind};
use crate::qemu::{Disk, MachineArgs};
use crate::{image, scenario};
use anyhow::{Result, bail};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// The controllers the boot disk is put on, with the name of each run.
const DISKS: [(&str, Disk); 2] = [("nvme", Disk::Nvme), ("ahci", Disk::Ahci)];
/// The line the test init logs once the root and the ESP are served.
const PASSED_LINE: &str = "SCENARIO root-disk PASSED";

/// Why the run did not pass, if it did not, given QEMU's exit code and
/// the serial output.
fn verdict(code: Option<i32>, serial: &str) -> Result<(), String> {
    crate::test::verdict(code, serial)?;
    if !serial.contains(PASSED_LINE) {
        return Err(format!("{PASSED_LINE:?} is missing"));
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
    for (name, disk) in DISKS {
        let dir = dir.join(name);
        fs::create_dir_all(&dir)?;
        image::create_with(kind, built, &dir.join("disk.img"), "test=root-disk", &[])?;
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
                "the {name} root disk run timed out; serial output in {}",
                log.display()
            );
        };
        let serial = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
        if let Err(why) = verdict(status.code(), &serial) {
            bail!(
                "the {name} root disk run failed: {why}; serial output in {}",
                log.display()
            );
        }
        println!("root disk run {name} passed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_the_passed_code_and_line() {
        let passed = crate::test::PASSED;
        assert_eq!(verdict(Some(passed), PASSED_LINE), Ok(()));
        assert!(verdict(Some(passed), "SCENARIO root-disk FAILED").is_err());
        assert!(verdict(Some(passed + 2), PASSED_LINE).is_err());
        assert!(verdict(None, PASSED_LINE).is_err());
        let panicked = format!("{PASSED_LINE}\nKERNEL PANIC: page fault");
        assert!(verdict(Some(passed), &panicked).is_err());
    }
}
