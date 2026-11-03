//! Runs of the boot menu: each boots the test image with an entries
//! file, or none, presses keys through the QEMU monitor, and checks
//! from the serial output what the boot menu started.

use crate::build::{self, Kind};
use crate::image;
use crate::qemu::MachineArgs;
use crate::scenario::{
    WAITING, monitor, monitor_socket, ppm_pixels, screen_dump, start, wait_for_line,
};
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
    /// The serial output gets this line, but not within 3 s of the
    /// firmware starting the boot menu.
    BootsLater(&'static str),
    /// The menu stays up, starting nothing.
    Stays,
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
    /// Another system's boot file put on the boot volume, if any.
    other: Option<&'static str>,
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

/// An entries file with a key that appears twice in one entry.
const INVALID: &str = "\
entries:
  - id: mikros
    id: again
    name: Mikros
    path: /EFI/Mikros/mikrosboot.efi
";

const RUNS: [Run; 7] = [
    // The countdown starts the default entry with its arguments as the
    // kernel command line.
    Run {
        name: "countdown",
        entries: Some(ONE_ENTRY_WITH_ARGS),
        other: None,
        hold: false,
        keys: &[],
        outcome: Outcome::Boots(WAITING),
    },
    // Without an entries file, another system on the boot volume makes
    // the menu count down 5 s before it starts Mikros.
    Run {
        name: "detected-system",
        entries: None,
        other: Some("EFI/Microsoft/Boot/bootmgfw.efi"),
        hold: false,
        keys: &[],
        outcome: Outcome::BootsLater(KERNEL_STARTS),
    },
    // An entries file with an error leaves the menu without entries.
    Run {
        name: "invalid-entries",
        entries: Some(INVALID),
        other: None,
        hold: false,
        keys: &[],
        outcome: Outcome::Stays,
    },
    // A held key shows the menu where the missing entries file would
    // start Mikros at once, and Enter starts the focused entry.
    Run {
        name: "held-key",
        entries: None,
        other: None,
        hold: true,
        keys: &["ret"],
        outcome: Outcome::Boots(KERNEL_STARTS),
    },
    // A default entry whose image is missing brings the menu back, from
    // where Right and Enter start the next entry.
    Run {
        name: "failed-entry",
        entries: Some(BROKEN_DEFAULT),
        other: None,
        hold: false,
        keys: &["right", "ret"],
        outcome: Outcome::Boots(KERNEL_STARTS),
    },
    // After the one card come Restart and Shut down.
    Run {
        name: "shutdown",
        entries: None,
        other: None,
        hold: true,
        keys: &["right", "right", "ret"],
        outcome: Outcome::PowersOff,
    },
    Run {
        name: "restart",
        entries: None,
        other: None,
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

/// Waits until the screen, dumped through the monitor on `socket` into
/// `dir`, shows something and stays the same for a second, as the menu
/// does once it is drawn and waits for keys, or fails once `deadline`
/// has passed or the kernel has started.
fn wait_for_menu(socket: &Path, dir: &Path, deadline: Instant) -> Result<()> {
    let path = dir.join("menu.ppm");
    let mut last: Option<Vec<u8>> = None;
    loop {
        let dump = screen_dump(socket, &path)?;
        let drawn = ppm_pixels(&dump).is_some_and(|p| p.iter().any(|&c| c != p[0]));
        if drawn && last.as_ref() == Some(&dump) {
            return Ok(());
        }
        if serial(&dir.join("serial.log")).contains(KERNEL_STARTS) {
            bail!("the boot menu started an entry without being asked");
        }
        if Instant::now() > deadline {
            bail!("the menu did not come up");
        }
        last = Some(dump);
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Boots the image in `dir` and checks that `run` ends as it must.
fn boot(machine: &MachineArgs, dir: &Path, run: &Run, timeout: Duration) -> Result<()> {
    let log = dir.join("serial.log");
    let socket = monitor_socket(dir);
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
            wait_for_menu(&socket, dir, deadline)?;
            for key in run.keys {
                monitor(&socket, &format!("sendkey {key}"))?;
                std::thread::sleep(KEY_GAP);
            }
        }
        match run.outcome {
            Outcome::Boots(line) => {
                wait_for_line(&log, line, &mut qemu, deadline)?;
            }
            Outcome::BootsLater(line) => {
                std::thread::sleep(Duration::from_secs(3));
                if serial(&log).contains(line) {
                    bail!("the menu started its entry without counting down");
                }
                wait_for_line(&log, line, &mut qemu, deadline)?;
            }
            Outcome::Stays => {
                std::thread::sleep(SETTLE);
                if serial(&log).contains(KERNEL_STARTS) || qemu.try_wait()?.is_some() {
                    bail!("the menu did not stay up");
                }
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
            .chain(run.other.map(|path| (path, &b"not an image"[..])))
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
