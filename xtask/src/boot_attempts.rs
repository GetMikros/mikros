//! The boot attempts run: a disk whose `@system/previous` names a copy of
//! the current system version boots with `test=boot-attempts` again and
//! again in one machine, whose firmware variables persist across the
//! restarts. The test init logs each boot's version and `MikrosBoot`, and
//! restarts the machine until the bootloader fell back to the previous
//! version, as it must once the current one has had all its tries.

use crate::build::{self, Kind};
use crate::image::root;
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Result, bail};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const PASSED: &str = "SCENARIO boot-attempts PASSED";
const BOOT: &str = "SCENARIO boot-attempts: booted ";

/// The boots there must be: the version booted and what `MikrosBoot`
/// holds then, for the current version `current` and the previous one
/// `previous`. The first boot writes the variable with two more tries,
/// the next two count them down, and the last falls back without
/// changing it.
fn expected(current: &str, previous: &str) -> Vec<String> {
    let trying = |booted: &str, tries: u32| {
        format!("{booted}, MikrosBoot names {current}, state 0, {tries} tries left")
    };
    vec![
        trying(current, 2),
        trying(current, 1),
        trying(current, 0),
        trying(previous, 0),
    ]
}

/// The boots that `serial` logs, in order.
fn boots(serial: &str) -> Vec<&str> {
    serial
        .lines()
        .filter_map(|line| line.split_once(BOOT).map(|(_, boot)| boot.trim_end()))
        .collect()
}

/// Boots the run of the components `built` of the build kind `kind` in
/// `dir`, and checks its boots, which may take `timeout` in all.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    image::create_with_previous(kind, built, &dir.join("disk.img"), "test=boot-attempts")?;
    let log = dir.join("serial.log");
    let mut qemu = scenario::start(machine, dir, true)?;
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
    let serial = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
    let want = expected(&root::version(kind), &root::previous_version(kind));
    let checked = match status {
        None => Err("it timed out".to_owned()),
        Some(status) => crate::test::verdict(status.code(), &serial),
    }
    .and_then(|()| match boots(&serial) == want {
        true => Ok(()),
        false => Err(format!("its boots were {:?}, not {want:?}", boots(&serial))),
    })
    .and_then(|()| match serial.contains(PASSED) {
        true => Ok(()),
        false => Err(format!("{PASSED:?} is missing")),
    });
    if let Err(why) = checked {
        bail!(
            "the boot attempts run failed: {why}; serial output in {}",
            log.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_boots_in_order() {
        let serial = "\
[1.0] I p3: SCENARIO boot-attempts: booted 1, MikrosBoot names 1, state 0, 2 tries left
[0.9] I kernel: unrelated
[1.0] I p3: SCENARIO boot-attempts: booted 0, MikrosBoot names 1, state 0, 0 tries left
";
        assert_eq!(
            boots(serial),
            [
                "1, MikrosBoot names 1, state 0, 2 tries left",
                "0, MikrosBoot names 1, state 0, 0 tries left"
            ]
        );
        let want = expected("1", "0");
        assert_eq!(want.len(), 4);
        assert_eq!(want[3], "0, MikrosBoot names 1, state 0, 0 tries left");
    }
}
