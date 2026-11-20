//! The desktop run: the system of the system tests without the session
//! settings, so that a desktop counts as installed, boots, the session
//! manager starts the compositor in greeter mode and gives it the
//! display, and the compositor shows its background. The run takes screen
//! dumps until the background covers the screen, and keeps the last in
//! `desktop.ppm`.

use crate::build::{self, Kind};
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// What the session manager logs once it started the compositor.
const STARTED: &str = "sessionmgr: started the compositor in greeter mode";
/// The compositor's background, [0.11, 0.12, 0.14] in the unsigned
/// normalized bytes of the display, and how far a pixel may be from it.
const BACKGROUND: [u8; 3] = [28, 31, 36];
const TOLERANCE: u8 = 2;
/// The share of the screen that the background must cover, with no
/// window on it.
const COVERED: f64 = 0.9;
/// How often the screen is dumped while the run waits for the background.
const DUMP_EVERY: Duration = Duration::from_secs(1);

/// The share of the pixels of the screen dump `ppm` that show the
/// background, or None when it is not a PPM image.
pub fn background_share(ppm: &[u8]) -> Option<f64> {
    let pixels = scenario::ppm_pixels(ppm)?;
    if pixels.is_empty() {
        return Some(0.0);
    }
    let near = |p: &[u8; 3]| {
        p.iter()
            .zip(BACKGROUND)
            .all(|(&a, b)| a.abs_diff(b) <= TOLERANCE)
    };
    let shown = pixels.iter().filter(|p| near(p)).count();
    Some(shown as f64 / pixels.len() as f64)
}

/// Boots the desktop run of the build kind `kind` in `dir`, and fails
/// unless the compositor's background covers the screen before `timeout`.
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
    let ended = (|| -> Result<f64> {
        scenario::wait_for_line(&log, STARTED, &mut qemu, deadline)?;
        let dump = dir.join("desktop.ppm");
        loop {
            let ppm = scenario::screen_dump(&socket, &dump)?;
            let share = background_share(&ppm).context("the screen dump is not a PPM image")?;
            if share >= COVERED {
                return Ok(share);
            }
            if qemu.try_wait()?.is_some() {
                bail!("QEMU exited before the background covered the screen");
            }
            if Instant::now() > deadline {
                bail!(
                    "timed out with the background on {:.0}% of the screen",
                    share * 100.0
                );
            }
            std::thread::sleep(DUMP_EVERY);
        }
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
    fn counts_the_pixels_near_the_background() {
        let screen = ppm(&[[28, 31, 36], [29, 30, 37], [0, 0, 0], [28, 31, 36]]);
        assert_eq!(background_share(&screen), Some(0.75));
        assert_eq!(background_share(b"not a picture"), None);
    }
}
