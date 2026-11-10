//! The console run: the test init serves `tests/console.img` with
//! `test=console`, on the test machine. The console probe in that image
//! starts the text console in the session manager's place and gives it
//! the display and the keyboards. The runner types a login through the
//! QEMU monitor, which the probe accepts; then, once the probe has
//! written to the terminal of the session, it checks that the screen
//! shows text, and types a line that the probe reads. The test init
//! stops QEMU through `isa-debug-exit` once the run passed or failed.

use crate::build::{self, Kind};
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

const SEAT: &str = "console-probe: gave the seat";
const LOGIN: &str = "console-probe: Login as sip";
const LINE: &str = "console-probe: waiting for a line";
const PASSED: &str = "SCENARIO console PASSED";
/// The keys of the login and of the line: the name, the password and
/// `ls`, each ended by Enter.
const NAME: &str = "s i p ret";
const PASSWORD: &str = "s e c r e t ret";
/// How long the console has to open the devices it was given.
const SETTLE: Duration = Duration::from_secs(3);

/// Sends the monitor command `sendkey` once for each of `keys`.
fn type_keys(socket: &Path, keys: &str) -> Result<()> {
    for key in keys.split(' ') {
        scenario::monitor(socket, &format!("sendkey {key}"))?;
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Why the run did not pass, if it did not, given QEMU's exit code, the
/// serial output and the screen dump taken in the session.
fn verdict(code: Option<i32>, serial: &str, screen: &[u8]) -> Result<(), String> {
    crate::test::verdict(code, serial)?;
    if !serial.contains(PASSED) {
        return Err(format!("{PASSED:?} is missing"));
    }
    match scenario::console_problem(screen) {
        Some(problem) => Err(problem.into()),
        None => Ok(()),
    }
}

/// Boots the console run of the components `built` of the build kind
/// `kind` in `dir`.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    image::create_with(kind, built, &dir.join("disk.img"), "test=console", &[])?;
    let log = dir.join("serial.log");
    let socket = scenario::monitor_socket(dir);
    let mut qemu = scenario::start(machine, dir, false)?;
    let deadline = Instant::now() + timeout;
    let ended = (|| -> Result<(ExitStatus, Vec<u8>)> {
        scenario::wait_for_line(&log, SEAT, &mut qemu, deadline)?;
        std::thread::sleep(SETTLE);
        type_keys(&socket, NAME)?;
        type_keys(&socket, PASSWORD)?;
        scenario::wait_for_line(&log, LOGIN, &mut qemu, deadline)?;
        scenario::wait_for_line(&log, LINE, &mut qemu, deadline)?;
        // The display driver copies what changed after a frame's time.
        std::thread::sleep(Duration::from_secs(1));
        let screen = scenario::screen_dump(&socket, &dir.join("screen.ppm"))?;
        type_keys(&socket, "l s ret")?;
        loop {
            if let Some(status) = qemu.try_wait()? {
                return Ok((status, screen));
            }
            if Instant::now() > deadline {
                bail!("timed out waiting for the run to end");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    let _ = qemu.kill();
    let _ = qemu.wait();
    let (status, screen) = ended
        .with_context(|| format!("the console run failed; serial output in {}", log.display()))?;
    let serial = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
    if let Err(why) = verdict(status.code(), &serial, &screen) {
        bail!(
            "the console run failed: {why}; serial output in {}",
            log.display()
        );
    }
    Ok(())
}
