//! The input run: the test init serves `tests/input.img` with
//! `test=input`, on the test machine, which has QEMU's PS/2 keyboard and
//! mouse and a USB keyboard behind a hub on an xHCI controller. The input probe in that
//! image owns the basic display driver's display and fills it with one
//! colour, which the screen must show; then it waits for keys and mouse
//! movement, which the runner sends through the QEMU monitor until the
//! probe has had A pressed on every keyboard and the mouse moved and
//! clicked. The USB keyboard, which QEMU sends keys to first, is removed
//! once it had A pressed, so that the PS/2 keyboard gets the next ones. The test init stops QEMU through `isa-debug-exit` once the
//! run passed or failed.

use crate::build::{self, Kind};
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const PAINTED: &str = "input-probe: painted";
const WAITING: &str = "input-probe: waiting for input";
const PROBE_PASSED: &str = "input-probe: PASSED";
const PASSED: &str = "SCENARIO input PASSED";
/// The colour the probe fills the screen with, as red, green and blue.
const PAINT: [u8; 3] = [0x12, 0x34, 0x56];
/// The monitor commands that send input: the A key, a move to the right
/// and down, and a left click.
const INPUT: [&str; 4] = [
    "sendkey a",
    "mouse_move 8 4",
    "mouse_button 1",
    "mouse_button 0",
];
/// The line the probe logs once the USB keyboard had A pressed, and the
/// QEMU ID of that keyboard, which `test::test_args` gives it. QEMU sends
/// keys to the USB keyboard while it is there, so it is removed then, and
/// the next keys go to the PS/2 keyboard.
const USB_PRESSED: &str = "input-probe: A pressed on QEMU USB Keyboard";
const USB_KEYBOARD: &str = "usbkbd";
/// How long to wait between rounds of input.
const ROUND: Duration = Duration::from_millis(500);

/// Why the run did not pass, if it did not, given QEMU's exit code, the
/// serial output and the screen dump taken after the probe painted.
fn verdict(code: Option<i32>, serial: &str, screen: &[u8]) -> Result<(), String> {
    crate::test::verdict(code, serial)?;
    if !serial.contains(PASSED) {
        return Err(format!("{PASSED:?} is missing"));
    }
    match scenario::ppm_pixels(screen) {
        None => Err("the screen dump is not a PPM image".into()),
        Some(pixels) if pixels.iter().all(|&p| p == PAINT) => Ok(()),
        Some(_) => Err("the screen does not show the probe's colour".into()),
    }
}

/// Boots the input run of the components `built` of the build kind
/// `kind` in `dir`.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    image::create_with(kind, built, &dir.join("disk.img"), "test=input", &[])?;
    let log = dir.join("serial.log");
    let socket = scenario::monitor_socket(dir);
    let mut qemu = scenario::start(machine, dir, false)?;
    let deadline = Instant::now() + timeout;
    let ended = (|| {
        scenario::wait_for_line(&log, PAINTED, &mut qemu, deadline)?;
        // The display driver copies a commit after a frame's time, and
        // slowly in a debug build, so the dump waits until it has.
        std::thread::sleep(Duration::from_secs(1));
        let screen = scenario::screen_dump(&socket, &dir.join("screen.ppm"))?;
        scenario::wait_for_line(&log, WAITING, &mut qemu, deadline)?;
        let mut removed = false;
        loop {
            if let Some(status) = qemu.try_wait()? {
                return Ok((status, screen));
            }
            if Instant::now() > deadline {
                bail!("timed out sending input");
            }
            let serial = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
            let mut commands = Vec::new();
            if !removed && serial.contains(USB_PRESSED) {
                commands.push(format!("device_del {USB_KEYBOARD}"));
                removed = true;
            }
            if !serial.contains(PROBE_PASSED) {
                commands.extend(INPUT.iter().map(|c| c.to_string()));
            }
            for command in commands {
                // QEMU may end the run, and close the monitor, meanwhile.
                if let Err(e) = scenario::monitor(&socket, &command) {
                    std::thread::sleep(Duration::from_secs(1));
                    match qemu.try_wait()? {
                        Some(status) => return Ok((status, screen)),
                        None => return Err(e),
                    }
                }
            }
            std::thread::sleep(ROUND);
        }
    })();
    let _ = qemu.kill();
    let _ = qemu.wait();
    let (status, screen) = ended
        .with_context(|| format!("the input run failed; serial output in {}", log.display()))?;
    let serial = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
    if let Err(why) = verdict(status.code(), &serial, &screen) {
        bail!(
            "the input run failed: {why}; serial output in {}",
            log.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ppm(pixel: [u8; 3]) -> Vec<u8> {
        let mut ppm = b"P6\n2 1\n255\n".to_vec();
        ppm.extend(pixel);
        ppm.extend(pixel);
        ppm
    }

    #[test]
    fn needs_the_passed_line_code_and_colour() {
        let passed = Some(crate::test::PASSED);
        assert_eq!(verdict(passed, PASSED, &ppm(PAINT)), Ok(()));
        assert!(verdict(passed, "SCENARIO input FAILED", &ppm(PAINT)).is_err());
        assert!(verdict(Some(crate::test::PASSED + 2), PASSED, &ppm(PAINT)).is_err());
        assert!(verdict(passed, PASSED, &ppm([0, 0, 0])).is_err());
        assert!(verdict(passed, PASSED, b"not an image").is_err());
    }
}
