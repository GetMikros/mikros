//! The run of the test image that ends with the machine powered off: the
//! test init serves `tests/power.img` with `test=power-off`, and once the
//! service manager watches the power button and the power probe runs,
//! the runner presses the button through QEMU's monitor. The service
//! manager then stops every service and powers the machine off through
//! the ACPI server, after which QEMU exits by itself. The device manager,
//! which runs the PCI bus driver with the drivers of the disks, their
//! partitions and the USB disk below it, must have removed every device
//! but the ACPI device when it stopped, each after the devices below
//! it.

use crate::build::{self, Kind};
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// The lines that the service manager logs once it watches the power
/// button, and the power probe once it runs, before the button is
/// pressed.
const READY: [&str; 2] = [
    "watching the power button",
    "power-probe: running until it is stopped",
];
/// The lines the serial port must show, in this order, before QEMU exits:
/// the services stop in the reverse of their start order, the probe
/// first and the permission broker last.
const SHUTDOWN: [&str; 5] = [
    "the power button was pressed: shutting down",
    "shutting down: stopping the services",
    "stopped service power-probe",
    "stopped service device-manager",
    "stopped service permissions",
];
const PANIC_LINE: &str = "KERNEL PANIC: ";
/// The device manager's lines once it got `TERM`, and once it removed
/// the devices, and how it logs a device it added or removed.
const STOPPING: &str = "device-manager: stopping, removing every device";
const REMOVED_ALL: &str = "device-manager: every device but the ACPI device removed, exiting";
const ADDED: &str = "device-manager: added device ";
const REMOVED: &str = "device-manager: removed device ";
const ACPI_DEVICE: &str = ", as platform:acpi,";
/// How deep below a root device the devices must reach, so that the
/// order of removal says something: disks below PCI functions, and
/// partitions below them.
const DEPTH: usize = 3;

/// The device ID at the start of `text`.
fn id(text: &str) -> Option<u64> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    text[..end].parse().ok()
}

/// Why the device manager's lines in `serial` do not show it removing
/// every device it had when it got `TERM`, or added while it removed
/// them, but the ACPI device, each once and after the devices below it,
/// before the service manager logged it stopped, if they do not.
fn removals(serial: &str) -> Result<(), String> {
    let start = serial
        .find(STOPPING)
        .ok_or("the device manager did not log that it was stopping")?;
    let end = serial[start..]
        .find(REMOVED_ALL)
        .map(|at| start + at)
        .ok_or("the device manager did not log that it removed the devices")?;
    if serial
        .find("stopped service device-manager")
        .is_none_or(|s| s < end)
    {
        return Err("the device manager stopped before it removed the devices".into());
    }
    // The devices that the device manager had when it got `TERM` or
    // added while it removed them: each one's parent, and whether it is
    // the ACPI device. Those removed before `TERM` are left out.
    let mut devices: BTreeMap<u64, (Option<u64>, bool)> = BTreeMap::new();
    let mut order = Vec::new();
    for (at_line, line) in serial[..end].split('\n').scan(0, |at, line| {
        let here = *at;
        *at += line.len() + 1;
        Some((here, line))
    }) {
        let stopping = at_line >= start;
        if let Some(at) = line.find(ADDED) {
            let rest = &line[at + ADDED.len()..];
            let Some(device) = id(rest) else { continue };
            let parent = rest.find(", under ").and_then(|u| id(&rest[u + 8..]));
            devices.insert(device, (parent, rest.contains(ACPI_DEVICE)));
        } else if let Some(at) = line.find(REMOVED) {
            let device = id(&line[at + REMOVED.len()..]).ok_or("a removal without a device")?;
            if !stopping {
                devices.remove(&device);
            } else if order.contains(&device) {
                return Err(format!("device {device} was removed twice"));
            } else {
                order.push(device);
            }
        }
    }
    let depth = |mut device: u64| {
        let mut depth = 0;
        while let Some(&(Some(parent), _)) = devices.get(&device) {
            (device, depth) = (parent, depth + 1);
        }
        depth
    };
    if devices.keys().map(|&d| depth(d)).max().unwrap_or(0) < DEPTH {
        return Err(format!("no device was {DEPTH} levels below a root device"));
    }
    for (&device, &(parent, acpi)) in &devices {
        let at = order.iter().position(|&d| d == device);
        match (acpi, at) {
            (true, Some(_)) => return Err("the ACPI device was removed".into()),
            (false, None) => return Err(format!("device {device} was not removed")),
            _ => {}
        }
        let parent_at = parent.and_then(|p| order.iter().position(|&d| d == p));
        if let (Some(at), Some(parent_at)) = (at, parent_at)
            && parent_at < at
        {
            return Err(format!("device {device} was removed after its parent"));
        }
    }
    if let Some(device) = order.iter().find(|d| !devices.contains_key(d)) {
        return Err(format!("device {device}, which was not there, was removed"));
    }
    Ok(())
}

/// Why `serial` does not show the machine shut down as it must, if it
/// does not, given whether QEMU exited successfully.
fn verdict(exited: bool, serial: &str) -> Result<(), String> {
    if serial.contains(PANIC_LINE) {
        return Err("the kernel panicked".into());
    }
    let mut rest = serial;
    for line in SHUTDOWN {
        match rest.find(line) {
            Some(at) => rest = &rest[at + line.len()..],
            None => return Err(format!("{line:?} is missing or out of order")),
        }
    }
    removals(serial)?;
    if !exited {
        return Err("QEMU did not exit successfully".into());
    }
    Ok(())
}

/// Boots the power-off run of the components `built` of the build kind
/// `kind` in `dir`, and checks how it ends.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    image::create_with(kind, built, &dir.join("disk.img"), "test=power-off", &[])?;
    let log = dir.join("serial.log");
    let socket = scenario::monitor_socket(dir);
    let mut qemu = scenario::start(machine, dir, false)?;
    let deadline = Instant::now() + timeout;
    let ended = (|| {
        for line in READY {
            scenario::wait_for_line(&log, line, &mut qemu, deadline)?;
        }
        scenario::monitor(&socket, "system_powerdown")?;
        // QEMU exits by itself once the machine is off.
        loop {
            if let Some(status) = qemu.try_wait()? {
                return Ok(status);
            }
            if Instant::now() > deadline {
                bail!("timed out waiting for the machine to power off");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    let _ = qemu.kill();
    let _ = qemu.wait();
    let status = ended.with_context(|| {
        format!(
            "the power-off run failed; serial output in {}",
            log.display()
        )
    })?;
    let serial = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
    if let Err(why) = verdict(status.success(), &serial) {
        bail!(
            "the power-off run failed: {why}; serial output in {}",
            log.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The device manager's lines of a machine with the ACPI device, a
    /// PCI host with a function, its disk with a partition, and a device
    /// that was removed before the shutdown, and its removals in `order`.
    fn devices(order: &[u64]) -> String {
        let mut lines = vec![
            "device-manager: added device 1, pci-host-0, as platform:pci-host-ecam, with 1 resource".to_owned(),
            "device-manager: added device 2, acpi, as platform:acpi, with 0 resources".into(),
            "device-manager: added device 3, rtc, as acpi:pnp0b00, under 2, with 2 resources".into(),
            "device-manager: added device 4, pci-0000:00:02.0, as pci:c010802, under 1".into(),
            "device-manager: added device 5, disk-0, as storage:disk, under 4".into(),
            "device-manager: added device 6, part-1, as storage:partition, under 5".into(),
            "device-manager: added device 7, part-2, as storage:partition, under 5".into(),
            "device-manager: removed device 7, part-2".into(),
            STOPPING.into(),
        ];
        lines.extend(order.iter().map(|d| format!("{REMOVED}{d}, x")));
        lines.push(REMOVED_ALL.into());
        lines.join("\n")
    }

    fn shutdown(devices: &str) -> String {
        format!(
            "{}\n{devices}\n{}",
            SHUTDOWN[..3].join("\n"),
            SHUTDOWN[3..].join("\n")
        )
    }

    #[test]
    fn needs_every_line_in_order_and_a_clean_exit() {
        let serial = shutdown(&devices(&[3, 6, 5, 4, 1]));
        assert_eq!(verdict(true, &serial), Ok(()));
        assert!(verdict(false, &serial).is_err());
        let mut swapped = SHUTDOWN;
        swapped.swap(3, 4);
        assert!(verdict(true, &swapped.join("\n")).is_err());
        assert!(verdict(true, &SHUTDOWN[..4].join("\n")).is_err());
        let panicked = format!("{serial}\nKERNEL PANIC: page fault");
        assert!(verdict(true, &panicked).is_err());
    }

    #[test]
    fn needs_every_device_but_acpi_removed_children_first() {
        assert_eq!(removals(&shutdown(&devices(&[6, 5, 4, 3, 1]))), Ok(()));
        let err = |order: &[u64]| removals(&shutdown(&devices(order))).unwrap_err();
        assert!(err(&[3, 6, 5, 4]).contains("device 1 was not removed"));
        assert!(err(&[3, 6, 5, 4, 1, 2]).contains("ACPI"));
        assert!(err(&[3, 5, 6, 4, 1]).contains("device 6 was removed after its parent"));
        assert!(err(&[3, 6, 5, 4, 1, 6]).contains("twice"));
        assert!(err(&[3, 6, 7, 5, 4, 1]).contains("device 7, which was not there"));
        // A device added while the others are removed must go too.
        let late = "device-manager: added device 8, part-3, as storage:partition, under 5";
        let with_late =
            |order: &[u64]| devices(order).replace(STOPPING, &format!("{STOPPING}\n{late}"));
        assert_eq!(removals(&shutdown(&with_late(&[3, 8, 6, 5, 4, 1]))), Ok(()));
        let err = removals(&shutdown(&with_late(&[3, 6, 5, 4, 1]))).unwrap_err();
        assert!(err.contains("device 8 was not removed"), "{err}");
        // The device manager must be done before the service manager says
        // it stopped.
        let early = format!("{}\n{}", SHUTDOWN.join("\n"), devices(&[3, 6, 5, 4, 1]));
        assert!(removals(&early).unwrap_err().contains("stopped before"));
        assert!(removals(&SHUTDOWN.join("\n")).is_err());
    }

    #[test]
    fn needs_devices_deep_enough_below_a_root() {
        let shallow = devices(&[3, 6, 5, 4, 1]).replace(", under 5", ", under 1");
        let err = removals(&shutdown(&shallow)).unwrap_err();
        assert!(err.contains("levels below"), "{err}");
    }
}
