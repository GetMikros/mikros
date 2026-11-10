//! The run of the test image that ends with the machine powered off: the
//! test init serves `tests/power.img` with `test=power-off`, and once the
//! service manager watches the power button and the power probe runs,
//! the runner presses the button through QEMU's monitor, again every
//! `PRESS_WAIT` until the service manager logs that it was pressed: its
//! connection reaches the ACPI server's events only once the server
//! serves it, a little after the service manager logs that it watches. The service
//! manager then stops every service and powers the machine off through
//! the ACPI server, after which QEMU exits by itself. The device manager,
//! which runs the PCI bus driver with the drivers of the disks, their
//! partitions and the USB disk below it, must have removed every device
//! but the ACPI device when it stopped, each after the devices below
//! it. The machine has an extra ACPI table, `PTS_TABLE`, whose method run
//! before power-off writes an I/O port that nothing used before, as some
//! firmware's does: the ACPI server can only reach it because it took
//! every operation region before the device manager exited.

use crate::build::{self, Kind};
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// The lines that the service manager logs once it watches the power
/// button, the power probe once it runs, and the device manager once it
/// added a disk's partition, a device `DEPTH` levels below a root
/// device, before the button is pressed.
const READY: [&str; 3] = [
    "watching the power button",
    "power-probe: running until it is stopped",
    ", part-1, as ",
];
/// The lines the serial port must show, in this order, before QEMU exits:
/// the services stop in the reverse of their start order, the probe
/// first and the permission broker last.
const SHUTDOWN: [&str; 6] = [
    "the power button was pressed: shutting down",
    "shutting down: stopping the services",
    "stopped service power-probe",
    "device-manager: the ACPI server prepared for shutdown",
    "stopped service devicemgr",
    "stopped service permissions",
];
/// An SSDT, assembled from the following, whose `\_PTS` writes an I/O
/// port and reads a byte of the ECAM range at `0xE0000000` through a
/// region that it creates as it runs, as some firmware's does. The read
/// only works while the device manager still grants resources, so the
/// run only powers off when the preparation for S5 runs before the
/// device manager exits:
///
/// ```text
/// DefinitionBlock ("", "SSDT", 2, "MIKROS", "PTSPORT", 1)
/// {
///     OperationRegion (PTSP, SystemIO, 0x80, 1)
///     Field (PTSP, ByteAcc, NoLock, Preserve)
///     {
///         PTSB, 8
///     }
///
///     Method (\_PTS, 1, Serialized)
///     {
///         OperationRegion (ECAM, SystemMemory, 0xE0000000, 1)
///         Field (ECAM, ByteAcc, NoLock, Preserve)
///         {
///             VEND, 8
///         }
///
///         PTSB = Arg0
///         Local0 = VEND
///     }
/// }
/// ```
const PTS_TABLE: [u8; 105] = [
    0x53, 0x53, 0x44, 0x54, 0x69, 0x00, 0x00, 0x00, 0x02, 0x56, 0x4d, 0x49, 0x4b, 0x52, 0x4f, 0x53,
    0x50, 0x54, 0x53, 0x50, 0x4f, 0x52, 0x54, 0x00, 0x01, 0x00, 0x00, 0x00, 0x49, 0x4e, 0x54, 0x4c,
    0x30, 0x09, 0x26, 0x20, 0x5b, 0x80, 0x50, 0x54, 0x53, 0x50, 0x01, 0x0a, 0x80, 0x01, 0x5b, 0x81,
    0x0b, 0x50, 0x54, 0x53, 0x50, 0x01, 0x50, 0x54, 0x53, 0x42, 0x08, 0x14, 0x2d, 0x5c, 0x5f, 0x50,
    0x54, 0x53, 0x09, 0x5b, 0x80, 0x45, 0x43, 0x41, 0x4d, 0x00, 0x0c, 0x00, 0x00, 0x00, 0xe0, 0x01,
    0x5b, 0x81, 0x0b, 0x45, 0x43, 0x41, 0x4d, 0x01, 0x56, 0x45, 0x4e, 0x44, 0x08, 0x70, 0x68, 0x50,
    0x54, 0x53, 0x42, 0x70, 0x56, 0x45, 0x4e, 0x44, 0x60,
];
const PANIC_LINE: &str = "KERNEL PANIC: ";
/// The ACPI server's line once it took its resources in
/// `PrepareShutdown`, which only reaches the serial port while the log
/// service runs; its line when the preparation for S5 failed; and the
/// line the service manager would log had it stopped the log service.
const HOLDING: &str = "acpi: holding ";
const PREPARE_FAILED: &str = "acpi: preparing S5 failed";
const LOG_STOPPED: &str = "stopped service log";
/// How long the runner waits for the service manager to see a press of
/// the button before it presses it again, and how often it presses.
const PRESS_WAIT: Duration = Duration::from_secs(5);
const PRESSES: u32 = 6;
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
        .find("stopped service devicemgr")
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
    if !serial.contains(HOLDING) {
        return Err(format!("{HOLDING:?} is missing"));
    }
    if serial.contains(PREPARE_FAILED) {
        return Err("the preparation for S5 failed".into());
    }
    if serial.contains(LOG_STOPPED) {
        return Err("the log service was stopped".into());
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
    let table = dir.join("pts.aml");
    fs::write(&table, PTS_TABLE)?;
    let extra = ["-acpitable".to_owned(), format!("file={}", table.display())];
    let mut qemu = scenario::start_with(machine, dir, false, &|_| Ok(()), &extra, false)?;
    let deadline = Instant::now() + timeout;
    let ended = (|| {
        for line in READY {
            scenario::wait_for_line(&log, line, &mut qemu, deadline)?;
        }
        for press in 1..=PRESSES {
            scenario::monitor(&socket, "system_powerdown")?;
            let until = (Instant::now() + PRESS_WAIT).min(deadline);
            match scenario::wait_for_line(&log, SHUTDOWN[0], &mut qemu, until) {
                Ok(_) => break,
                Err(_) if qemu.try_wait()?.is_some() => break,
                Err(e) if press == PRESSES => return Err(e),
                Err(_) => {}
            }
        }
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
            "{}\n{devices}\n{HOLDING}2 operation regions\n{}",
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
    fn needs_the_acpi_server_prepared_and_the_log_service_kept() {
        let serial = shutdown(&devices(&[3, 6, 5, 4, 1]));
        let without = serial.replace(HOLDING, "acpi: ");
        assert!(verdict(true, &without).unwrap_err().contains("missing"));
        let failed = format!("{serial}\n{PREPARE_FAILED}: 0x4");
        assert!(verdict(true, &failed).unwrap_err().contains("S5"));
        let stopped = format!("{serial}\n{LOG_STOPPED}");
        assert!(verdict(true, &stopped).unwrap_err().contains("log service"));
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
    fn has_a_valid_table() {
        assert_eq!(&PTS_TABLE[..4], b"SSDT");
        let length = u32::from_le_bytes(PTS_TABLE[4..8].try_into().unwrap());
        assert_eq!(length as usize, PTS_TABLE.len());
        let sum = PTS_TABLE.iter().fold(0u8, |sum, &b| sum.wrapping_add(b));
        assert_eq!(sum, 0);
    }

    #[test]
    fn needs_devices_deep_enough_below_a_root() {
        let shallow = devices(&[3, 6, 5, 4, 1]).replace(", under 5", ", under 1");
        let err = removals(&shutdown(&shallow)).unwrap_err();
        assert!(err.contains("levels below"), "{err}");
    }
}
