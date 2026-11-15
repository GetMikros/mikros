//! The runs whose systems have a root partition of their own: the test
//! init serves a test image with the root disk's drivers, and the root
//! partition holds the image's files, so that the process manager builds
//! namespaces from it.

use crate::build::{self, Kind};
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Result, bail};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// Boots the scenario `name` of the components `built` of the build kind
/// `kind` in `dir`, with the files of the test image `<name>.img` on the
/// root partition, and checks that it passed within `timeout`.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
    name: &str,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    let system = build::test_image_path(kind, &format!("{name}.img"));
    let command_line = format!("test={name}");
    image::create_with_system(kind, built, &system, &dir.join("disk.img"), &command_line)?;
    let log = dir.join("serial.log");
    let mut qemu = scenario::start(machine, dir, false)?;
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
            "the {name} run timed out; serial output in {}",
            log.display()
        );
    };
    let serial = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
    let passed = format!("SCENARIO {name} PASSED");
    let checked = crate::test::verdict(status.code(), &serial).and_then(|()| {
        match serial.contains(&passed) {
            true => Ok(()),
            false => Err(format!("{passed:?} is missing")),
        }
    });
    if let Err(why) = checked {
        bail!(
            "the {name} run failed: {why}; serial output in {}",
            log.display()
        );
    }
    Ok(())
}
