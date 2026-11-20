//! The desktop run: the system of the system tests without the session
//! settings, so that a desktop counts as installed, and with shell
//! settings that name the greeter probe as the greeter, boots, the
//! session manager starts the compositor in greeter mode and gives it the
//! display, and the compositor starts the probe, a system app, which
//! draws a fullscreen toplevel in one colour with Vulkan. The run takes
//! screen dumps until that colour covers the screen. Then it presses
//! Ctrl, Alt and F2, which the compositor turns into a switch to the
//! screen of text console 1, waits until the colour has left the screen,
//! and presses Ctrl, Alt and F1, which the console turns into a switch
//! back to the desktop, until the colour covers the screen again. It
//! keeps the last screen dump in `desktop.ppm`.

use crate::build::{self, Kind};
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// What the session manager logs once it started the compositor, and the
/// compositor once it started the greeter.
const STARTED: &str = "sessionmgr: started the compositor in greeter mode";
const GREETER: &str = "compositor: started the greeter as pid";
/// The probe's colour, in the bytes of the display, and how far a pixel
/// may be from it.
const COLOUR: [u8; 3] = [200, 40, 120];
const TOLERANCE: u8 = 2;
/// The share of the screen that the colour must cover, and the share
/// that it may still cover on a text console's screen.
const COVERED: f64 = 0.9;
const GONE: f64 = 0.1;
/// How often the screen is dumped while the run waits for the colour.
const DUMP_EVERY: Duration = Duration::from_secs(1);

/// The share of the pixels of the screen dump `ppm` that show the probe's
/// colour, or None when it is not a PPM image.
pub fn colour_share(ppm: &[u8]) -> Option<f64> {
    let pixels = scenario::ppm_pixels(ppm)?;
    if pixels.is_empty() {
        return Some(0.0);
    }
    let near = |p: &[u8; 3]| {
        p.iter()
            .zip(COLOUR)
            .all(|(&a, b)| a.abs_diff(b) <= TOLERANCE)
    };
    let shown = pixels.iter().filter(|p| near(p)).count();
    Some(shown as f64 / pixels.len() as f64)
}

/// Boots the desktop run of the build kind `kind` in `dir`, and fails
/// unless the probe's colour covers the screen before `timeout`.
pub fn run(machine: &MachineArgs, kind: Kind, dir: &Path, timeout: Duration) -> Result<()> {
    let built = build::build_tests(kind)?;
    let system = built
        .desktop_image
        .as_deref()
        .context("the build of the system tests has no desktop system")?;
    fs::create_dir_all(dir)?;
    let disk = dir.join("disk.img");
    image::create_with_system(kind, &built, system, &disk, "")?;
    let log = dir.join("serial.log");
    let socket = scenario::monitor_socket(dir);
    let mut qemu = scenario::start(machine, dir, false)?;
    let deadline = Instant::now() + timeout;
    let ended = (|| -> Result<()> {
        scenario::wait_for_line(&log, STARTED, &mut qemu, deadline)?;
        scenario::wait_for_line(&log, GREETER, &mut qemu, deadline)?;
        let dump = dir.join("desktop.ppm");
        // Until the share of the colour on the screen is as `wanted`
        // says, which `what` names.
        let mut until = |wanted: &dyn Fn(f64) -> bool, what: &str| -> Result<()> {
            loop {
                let ppm = scenario::screen_dump(&socket, &dump)?;
                let share = colour_share(&ppm).context("the screen dump is not a PPM image")?;
                if wanted(share) {
                    return Ok(());
                }
                if qemu.try_wait()?.is_some() {
                    bail!("QEMU exited before {what}");
                }
                if Instant::now() > deadline {
                    bail!(
                        "timed out before {what}, with the probe's colour on {:.0}% of the screen",
                        share * 100.0
                    );
                }
                std::thread::sleep(DUMP_EVERY);
            }
        };
        until(
            &|share| share >= COVERED,
            "the probe's colour covered the screen",
        )?;
        scenario::monitor(&socket, "sendkey ctrl-alt-f2")?;
        until(&|share| share <= GONE, "the text console's screen showed")?;
        scenario::monitor(&socket, "sendkey ctrl-alt-f1")?;
        until(&|share| share >= COVERED, "the desktop showed again")
    })();
    let _ = qemu.kill();
    let _ = qemu.wait();
    ended.with_context(|| {
        format!(
            "the desktop run failed; serial output in {}, the screen in {}",
            log.display(),
            dir.join("desktop.ppm").display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ppm(pixels: &[[u8; 3]]) -> Vec<u8> {
        let mut ppm = format!("P6\n{} 1\n255\n", pixels.len()).into_bytes();
        ppm.extend(pixels.iter().flatten());
        ppm
    }

    #[test]
    fn counts_the_pixels_near_the_colour() {
        let screen = ppm(&[[200, 40, 120], [201, 39, 122], [28, 31, 36], [200, 40, 120]]);
        assert_eq!(colour_share(&screen), Some(0.75));
        assert_eq!(colour_share(b"not a picture"), None);
    }
}
