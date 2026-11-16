//! The runs whose systems have a root partition of their own: the test
//! init serves the setup run's image, and the root partition holds the
//! files of the run's archive, so that the process manager builds
//! namespaces from it. The live run boots with `live`, and leaves the
//! root partition as it was.

use crate::build::{self, Kind};
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Context, Result, bail};
use mikrosboot::btrfs::Volume;
use mikrosboot::disk::{Disk, IoError, Slice};
use mikrosboot::gpt;
use std::fs::{self, File};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::time::{Duration, Instant};

/// Boots the scenario `name` of the components `built` of the build kind
/// `kind` in `dir`, with the files of the archive `<name>.img` on the
/// root partition and `extra` on the command line, and checks that it
/// passed within `timeout`.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
    name: &str,
    extra: &str,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    let system = build::test_image_path(kind, &format!("{name}.img"));
    let command_line = format!("test={name} {extra}");
    let command_line = command_line.trim();
    image::create_with_system(kind, built, &system, &dir.join("disk.img"), command_line)?;
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

/// A disk image file.
struct Image(File);

impl Disk for Image {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        self.0.read_exact_at(buf, offset).map_err(|_| IoError(0))
    }
}

/// The contents of `<name>` in the subvolume `config`, which is
/// `/system/config`, on the root partition of the disk image `disk`, or
/// `None` without such a file, read with the bootloader's readers.
fn root_config_file(disk: &Path, name: &str) -> Result<Option<Vec<u8>>> {
    const SECTOR: u32 = 512;
    let file = File::open(disk).with_context(|| format!("opening {}", disk.display()))?;
    let last = file.metadata()?.len() / u64::from(SECTOR) - 1;
    let mut image = Image(file);
    let partitions =
        gpt::partitions(&mut image, SECTOR, last).map_err(|e| anyhow::anyhow!("{e}"))?;
    let root = gpt::system_partition(&partitions, None).map_err(|e| anyhow::anyhow!("{e}"))?;
    let (start, size) = root
        .range(SECTOR, last)
        .context("the root partition is not on the disk")?;
    let disk = Slice {
        disk: image,
        start,
        size,
    };
    let mut volume = Volume::open(disk).map_err(|e| anyhow::anyhow!("{e}"))?;
    let fail = |e: mikrosboot::btrfs::Error| anyhow::anyhow!("{e}");
    let top = volume.top().map_err(fail)?;
    let config = volume
        .lookup(top, b"config")
        .map_err(fail)?
        .context("no config subvolume")?;
    let Some(node) = volume.lookup(config, name.as_bytes()).map_err(fail)? else {
        return Ok(None);
    };
    let mut contents = vec![0; volume.inode(node).map_err(fail)?.size as usize];
    volume.read(node, &mut contents).map_err(fail)?;
    Ok(Some(contents))
}

/// Boots the live run of the components `built` of the build kind `kind`
/// in `dir`, whose probe changes `/system/config`, and checks that the
/// root partition's `config` subvolume is as it was.
pub fn run_live(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    run(machine, kind, built, dir, timeout, "live", "live")?;
    let disk = dir.join("disk.img");
    let kept = root_config_file(&disk, "live-probe.conf")?;
    let made = root_config_file(&disk, "live-probe.new")?;
    if kept.as_deref() != Some(&b"lower\n"[..]) || made.is_some() {
        bail!(
            "the live run changed /system/config on its root partition, in {}",
            disk.display()
        );
    }
    Ok(())
}
