//! Runs of the boot menu: each boots the test image with an entries
//! file, or none, presses keys through the QEMU monitor, and checks
//! from the serial output what the boot menu started.

use crate::build::{self, Kind};
use crate::image;
use crate::qemu::MachineArgs;
use crate::scenario::{WAITING, monitor, start, wait_for_line};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

/// The line the firmware writes as it starts the boot menu.
const FIRMWARE_STARTS: &str = "BdsDxe: starting Boot";
/// The kernel's first line.
const KERNEL_STARTS: &str = "Mikros kernel";
/// How long the boot menu is given to start an entry on its own.
const SETTLE: Duration = Duration::from_secs(4);
/// The time between two key presses.
const KEY_GAP: Duration = Duration::from_millis(700);

/// What a run must end in.
enum Outcome {
    /// The serial output gets this line.
    Boots(&'static str),
    /// QEMU exits without the kernel starting.
    PowersOff,
    /// The firmware starts the boot menu again.
    Restarts,
}

/// A run of the boot menu.
struct Run {
    name: &'static str,
    /// The entries file, or `None` for none.
    entries: Option<&'static str>,
    /// Whether a key is pressed again and again while the boot menu
    /// starts.
    hold: bool,
    /// The keys pressed, in QEMU's names, once the menu is up.
    keys: &'static [&'static str],
    outcome: Outcome,
}

const ONE_ENTRY_WITH_ARGS: &str = "\
timeout: 1
entries:
  - id: mikros
    name: Mikros
    path: /EFI/Mikros/mikrosboot.efi
    args: \"test=wait\"
";

const BROKEN_DEFAULT: &str = "\
timeout: 0
entries:
  - id: broken
    name: Broken
    path: /EFI/none.efi
  - id: mikros
    name: Mikros
    path: \\EFI\\Mikros\\mikrosboot.efi
";

const RUNS: [Run; 5] = [
    // The countdown starts the default entry with its arguments as the
    // kernel command line.
    Run {
        name: "countdown",
        entries: Some(ONE_ENTRY_WITH_ARGS),
        hold: false,
        keys: &[],
        outcome: Outcome::Boots(WAITING),
    },
    // A held key shows the menu where the missing entries file would
    // start Mikros at once, and Enter starts the focused entry.
    Run {
        name: "held-key",
        entries: None,
        hold: true,
        keys: &["ret"],
        outcome: Outcome::Boots(KERNEL_STARTS),
    },
    // A default entry whose image is missing brings the menu back, from
    // where Right and Enter start the next entry.
    Run {
        name: "failed-entry",
        entries: Some(BROKEN_DEFAULT),
        hold: false,
        keys: &["right", "ret"],
        outcome: Outcome::Boots(KERNEL_STARTS),
    },
    // After the one card come Restart and Shut down.
    Run {
        name: "shutdown",
        entries: None,
        hold: true,
        keys: &["right", "right", "ret"],
        outcome: Outcome::PowersOff,
    },
    Run {
        name: "restart",
        entries: None,
        hold: true,
        keys: &["right", "ret"],
        outcome: Outcome::Restarts,
    },
];

fn serial(log: &Path) -> String {
    String::from_utf8_lossy(&fs::read(log).unwrap_or_default()).into_owned()
}

/// Presses `a` through the monitor on `socket` until the firmware has
/// started the boot menu, and for 2 s more, so that the boot menu finds a
/// key stroke as it starts.
fn hold_key(socket: &Path, log: &Path, qemu: &mut Child, deadline: Instant) -> Result<()> {
    let mut until = None;
    while until.is_none_or(|u| Instant::now() < u) {
        if until.is_none() && serial(log).contains(FIRMWARE_STARTS) {
            until = Some(Instant::now() + Duration::from_secs(2));
        }
        if qemu.try_wait()?.is_some() || Instant::now() > deadline {
            bail!("the boot menu did not start");
        }
        // The monitor may not listen yet right after QEMU starts.
        let _ = monitor(socket, "sendkey a");
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// Boots the image in `dir` and checks that `run` ends as it must.
fn boot(machine: &MachineArgs, dir: &Path, run: &Run, timeout: Duration) -> Result<()> {
    let log = dir.join("serial.log");
    let socket = dir.join("monitor.sock");
    let reboot = matches!(run.outcome, Outcome::Restarts);
    let mut qemu = start(machine, dir, reboot)?;
    let deadline = Instant::now() + timeout;
    let result = (|| {
        if run.hold {
            hold_key(&socket, &log, &mut qemu, deadline)?;
        } else {
            wait_for_line(&log, FIRMWARE_STARTS, &mut qemu, deadline)?;
        }
        if !run.keys.is_empty() {
            std::thread::sleep(SETTLE);
            if serial(&log).contains(KERNEL_STARTS) {
                bail!("the boot menu started an entry without being asked");
            }
            for key in run.keys {
                monitor(&socket, &format!("sendkey {key}"))?;
                std::thread::sleep(KEY_GAP);
            }
        }
        match run.outcome {
            Outcome::Boots(line) => {
                wait_for_line(&log, line, &mut qemu, deadline)?;
            }
            Outcome::PowersOff => loop {
                if qemu.try_wait()?.is_some() {
                    if serial(&log).contains(KERNEL_STARTS) {
                        bail!("the kernel started before the machine powered off");
                    }
                    break;
                }
                if Instant::now() > deadline {
                    bail!("the machine did not power off");
                }
                std::thread::sleep(Duration::from_millis(100));
            },
            Outcome::Restarts => loop {
                if serial(&log).matches(FIRMWARE_STARTS).count() >= 2 {
                    break;
                }
                if qemu.try_wait()?.is_some() || Instant::now() > deadline {
                    bail!("the firmware did not start the boot menu again");
                }
                std::thread::sleep(Duration::from_millis(100));
            },
        }
        Ok(())
    })();
    let _ = qemu.kill();
    let _ = qemu.wait();
    result
}

/// Runs every boot menu run on the components `built` of the build kind
/// `kind`, under `dir`.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    for run in &RUNS {
        let dir = dir.join(run.name);
        fs::create_dir_all(&dir)?;
        let files: Vec<(&str, &[u8])> = run
            .entries
            .map(|e| ("EFI/Mikros/entries.yaml", e.as_bytes()))
            .into_iter()
            .collect();
        image::create_with(kind, built, &dir.join("disk.img"), "", &files)?;
        boot(machine, &dir, run, timeout).with_context(|| {
            format!(
                "boot menu run {} failed; serial output in {}",
                run.name,
                dir.join("serial.log").display()
            )
        })?;
        println!("boot menu run {} ended as it should", run.name);
    }
    Ok(())
}
