//! The run of the test image that ends with the machine powered off: the
//! test init serves `tests/power.img` with `test=power-off`, and once its
//! power probe waits for the power button, the runner presses it through
//! QEMU's monitor. The probe then has the service manager stop every
//! service and power the machine off through the ACPI server, after which
//! QEMU exits by itself.

use crate::build::{self, Kind};
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// The line the power probe logs once it waits for the power button.
const WAITING: &str = "power-probe: waiting for the power button";
/// The lines the serial port must show, in this order, before QEMU exits:
/// the services stop in the reverse of their start order, the probe
/// first and the permission broker last.
const SHUTDOWN: [&str; 5] = [
    "power-probe: the power button was pressed",
    "shutting down: stopping the services",
    "stopped service power-probe",
    "stopped service device-manager",
    "stopped service permissions",
];
const PANIC_LINE: &str = "KERNEL PANIC: ";

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
        scenario::wait_for_line(&log, WAITING, &mut qemu, deadline)?;
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

    #[test]
    fn needs_every_line_in_order_and_a_clean_exit() {
        let serial = SHUTDOWN.join("\n");
        assert_eq!(verdict(true, &serial), Ok(()));
        assert!(verdict(false, &serial).is_err());
        let mut swapped = SHUTDOWN;
        swapped.swap(3, 4);
        assert!(verdict(true, &swapped.join("\n")).is_err());
        assert!(verdict(true, &SHUTDOWN[..4].join("\n")).is_err());
        let panicked = format!("{serial}\nKERNEL PANIC: page fault");
        assert!(verdict(true, &panicked).is_err());
    }
}
