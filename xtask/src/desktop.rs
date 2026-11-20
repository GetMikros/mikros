//! The desktop run: the system of the system tests without the session
//! settings, so that a desktop counts as installed, and with shell
//! settings that name the greeter probe as the greeter, boots, the
//! session manager starts the compositor in greeter mode and gives it the
//! display, and the compositor starts the probe, a system app, which
//! draws a fullscreen toplevel in one colour with Vulkan, and then logs
//! in `dee`, whose login mode is `CONSOLE`. The run waits until dee's
//! session started on text console 1 and its screen shows, without the
//! colour. Then it presses Ctrl, Alt and F1, which the console turns into
//! a switch to the desktop, until the colour covers the screen, and
//! Ctrl, Alt and F2, which the compositor turns into a switch back to the
//! console, until the colour has left the screen again. In dee's session
//! it then runs `desktop`, which starts dee's desktop: the run waits until
//! the compositor's background covers the screen, presses Ctrl, Alt and
//! F2 until it has left it, and ends the session with `exit`, which ends
//! the desktop too. It keeps the last screen dump in `desktop.ppm`.

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
/// What the session manager logs once the session of `dee`, whom the
/// probe logs in, started, and once dee's desktop started and ended.
const LOGGED_IN: &str = "sessionmgr: dee logged in on console 1";
const DESKTOP_STARTED: &str = "sessionmgr: started the desktop of dee";
const DESKTOP_ENDED: &str = "sessionmgr: the desktop of dee ended";
/// The probe's colour and the compositor's background, [0.11, 0.12,
/// 0.14] in unsigned normalized bytes, in the bytes of the display, and
/// how far a pixel may be from them.
const COLOUR: [u8; 3] = [200, 40, 120];
const BACKGROUND: [u8; 3] = [28, 31, 36];
const TOLERANCE: u8 = 2;
/// The share of the screen that the colour must cover, and the share
/// that it may still cover on a text console's screen.
const COVERED: f64 = 0.9;
const GONE: f64 = 0.1;
/// How often the screen is dumped while the run waits for the colour.
const DUMP_EVERY: Duration = Duration::from_secs(1);

/// The share of the pixels of the screen dump `ppm` that show `colour`,
/// or None when it is not a PPM image.
pub fn colour_share(ppm: &[u8], colour: [u8; 3]) -> Option<f64> {
    let pixels = scenario::ppm_pixels(ppm)?;
    if pixels.is_empty() {
        return Some(0.0);
    }
    let near = |p: &[u8; 3]| {
        p.iter()
            .zip(colour)
            .all(|(&a, b)| a.abs_diff(b) <= TOLERANCE)
    };
    let shown = pixels.iter().filter(|p| near(p)).count();
    Some(shown as f64 / pixels.len() as f64)
}

/// The screen of a run: its monitor's socket, the file of its dumps, and
/// when the run must have ended.
struct Screen<'a> {
    socket: &'a Path,
    dump: &'a Path,
    deadline: Instant,
}

impl Screen<'_> {
    /// Dumps the screen until the share of `colour` on it is as `wanted`
    /// says, which `what` names, while `qemu` runs.
    fn until(
        &mut self,
        qemu: &mut std::process::Child,
        colour: [u8; 3],
        wanted: &dyn Fn(f64) -> bool,
        what: &str,
    ) -> Result<()> {
        loop {
            let ppm = scenario::screen_dump(self.socket, self.dump)?;
            let share = colour_share(&ppm, colour).context("the screen dump is not a PPM image")?;
            if wanted(share) {
                return Ok(());
            }
            if qemu.try_wait()?.is_some() {
                bail!("QEMU exited before {what}");
            }
            if Instant::now() > self.deadline {
                bail!(
                    "timed out before {what}, with the colour on {:.0}% of the screen",
                    share * 100.0
                );
            }
            std::thread::sleep(DUMP_EVERY);
        }
    }
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
        scenario::wait_for_line(&log, LOGGED_IN, &mut qemu, deadline)?;
        let dump = dir.join("desktop.ppm");
        let mut screen = Screen {
            socket: &socket,
            dump: &dump,
            deadline,
        };
        let gone = |share: f64| share <= GONE;
        let covered = |share: f64| share >= COVERED;
        screen.until(&mut qemu, COLOUR, &gone, "the session's console showed")?;
        scenario::monitor(&socket, "sendkey ctrl-alt-f1")?;
        screen.until(&mut qemu, COLOUR, &covered, "the desktop showed")?;
        scenario::monitor(&socket, "sendkey ctrl-alt-f2")?;
        screen.until(&mut qemu, COLOUR, &gone, "the console showed again")?;
        crate::console::type_keys(&socket, "d e s k t o p ret")?;
        scenario::wait_for_line(&log, DESKTOP_STARTED, &mut qemu, deadline)?;
        let what = "dee's desktop showed";
        screen.until(&mut qemu, BACKGROUND, &covered, what)?;
        scenario::monitor(&socket, "sendkey ctrl-alt-f2")?;
        let what = "the console showed from dee's desktop";
        screen.until(&mut qemu, BACKGROUND, &gone, what)?;
        crate::console::type_keys(&socket, "e x i t ret")?;
        scenario::wait_for_line(&log, DESKTOP_ENDED, &mut qemu, deadline).map(drop)
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
        assert_eq!(colour_share(&screen, COLOUR), Some(0.75));
        assert_eq!(colour_share(&screen, BACKGROUND), Some(0.25));
        assert_eq!(colour_share(b"not a picture", COLOUR), None);
    }
}
