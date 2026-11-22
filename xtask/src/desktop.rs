//! The desktop run: the system of the system tests without the session
//! settings, so that a desktop counts as installed, and with shell
//! settings that name the greeter probe as the greeter, boots, the
//! session manager starts the compositor in greeter mode and gives it
//! the display, and the compositor starts the probe, a system app,
//! which draws a fullscreen toplevel in one colour with Vulkan, and
//! then logs in `dee`, whose login mode is `CONSOLE`. The run waits
//! until dee's session started on text console 1 and its screen shows,
//! without the colour. Then it presses Ctrl, Alt and F1, which the
//! console turns into a switch to the desktop, until the colour covers
//! the screen, clicks the probe's toplevel, which gives it the keyboard,
//! and types K, until the probe's second colour covers the screen, and
//! presses Ctrl, Alt and F2, which the compositor turns into a switch
//! back to the console, until that colour has left the screen again. In dee's session it then runs `desktop`, which starts dee's
//! desktop, whose shell is the shell probe: the run waits until the
//! wallpaper that its desktop part hands the compositor covers the
//! screen, until the haze of that wallpaper shows under its taskbar
//! part's transparent panel, until its switcher part's SDL window fills
//! the middle of the screen, where the compositor places it, with the
//! pointer's arrow over it, until the arrow follows QEMU's mouse onto the
//! wallpaper and turns into the hand that the panel asks for on the
//! panel, and until its start-menu part connected to the service
//! manager's `Control`, whose question the compositor cancels, and was
//! refused. It then presses Ctrl, Alt and F2 until the wallpaper has
//! left the screen, and ends the session with `exit`, which ends the
//! desktop too. It keeps the last screen dump in `desktop.ppm`.

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
/// What the compositor logs once it cancelled the shell probe's
/// question, and once the shell probe exited, which it does with 0 when
/// it was refused.
const CANCELLED: &str = "compositor: cancelled the question";
const SHELL_EXITED: &str = "compositor: the part start-menu of system.mikros.shell_probe exited";
/// What the compositor logs when it starts that part again, as it does
/// each time the part exits, and when the part has exited too often
/// within a minute, which the probe, that exits at once, does.
const SHELL_RESTARTED: &str =
    "compositor: starting the part start-menu of system.mikros.shell_probe again";
const SHELL_GAVE_UP: &str = "compositor: the part start-menu of system.mikros.shell_probe stopped more than 5 times within a minute; leaving its roles empty";
/// The probe's colour and the test colour of the shell probe's
/// wallpaper, in the bytes of the display, and how far a pixel may be
/// from them.
const COLOUR: [u8; 3] = [200, 40, 120];
const TYPED: [u8; 3] = [40, 170, 90];
const WALLPAPER: [u8; 3] = [40, 90, 160];
/// The haze of that wallpaper in the light scheme, which the shell
/// probe's transparent taskbar shows along the bottom edge, 48 rows of
/// the 800 of the screen, and the bottom part of the screen that it must
/// cover.
const HAZE: [u8; 3] = [210, 220, 235];
const HAZE_PART: f64 = 0.05;
/// A pixel inside the black fill of the cursor theme's default arrow,
/// which the compositor draws with its hotspot in the middle of the
/// screen until it reads pointer devices: 2 right of the hotspot and 7
/// below it.
const ARROW_PIXEL: (usize, usize) = (642, 407);
/// The moves of the pointer through QEMU's mouse, from the middle onto
/// the wallpaper and then onto the taskbar's panel, and pixels that the
/// cursor makes black after each: the arrow's fill, 2 right of and 7
/// below its hotspot, and the palm of the hand that the panel asks for,
/// 14 below its hotspot, where the arrow has its white outline.
const MOVE_TO_WALLPAPER: &str = "mouse_move 100 100";
const MOVED_ARROW_PIXEL: (usize, usize) = (742, 507);
const MOVE_TO_PANEL: &str = "mouse_move 0 270";
const HAND_PIXEL: (usize, usize) = (740, 784);
/// What the compositor logs when the shell probe's switcher part ends,
/// which it must not do before its window showed, with its failed step's
/// number after it if it failed.
const SDL_EXITED: &str = "compositor: the part switcher of system.mikros.shell_probe exited";
const TOLERANCE: u8 = 2;
/// The share of the screen that the colour must cover, and the share
/// that it may still cover on a text console's screen.
const COVERED: f64 = 0.9;
const GONE: f64 = 0.1;
/// The share of its window that the window's colour must cover: all of
/// it but the pointer's arrow.
const WINDOW_SHARE: f64 = 0.97;
/// How often the screen is dumped while the run waits for the colour.
const DUMP_EVERY: Duration = Duration::from_secs(1);

/// The window that the shell probe's switcher part shows, which the
/// compositor centres on the screen: its colour, its size, and what the
/// run is waiting for while it is not shown.
pub struct Window {
    pub colour: [u8; 3],
    pub size: (usize, usize),
    pub what: &'static str,
}

/// The window of the SDL probe, which the desktop run's system has by
/// default.
const SDL: Window = Window {
    colour: [230, 150, 20],
    size: (128, 96),
    what: "the SDL window showed",
};

/// The share of the pixels of the last `part` of the screen dump `ppm`,
/// its bottom rows, or all of it for 1, that show `colour`, or None when
/// it is not a PPM image.
fn colour_share(ppm: &[u8], colour: [u8; 3], part: f64) -> Option<f64> {
    let all = scenario::ppm_pixels(ppm)?;
    let pixels = &all[all.len() - (all.len() as f64 * part) as usize..];
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

/// The share of the pixels of the `width` by `height` rectangle in the
/// middle of the screen dump `ppm`, where the compositor places the first
/// window of a screen whose work area is all of it, that show `colour`,
/// or None when it is not a PPM image or has no such rectangle.
fn window_share(ppm: &[u8], colour: [u8; 3], window: (usize, usize)) -> Option<f64> {
    window_pixels(ppm, colour, window, false)
}

/// As `window_share`, for the pixels on the window's border, its first and
/// last rows and columns, which the pointer's arrow does not reach, so that
/// a window a pixel off the middle is not taken for it.
fn window_border_share(ppm: &[u8], colour: [u8; 3], window: (usize, usize)) -> Option<f64> {
    window_pixels(ppm, colour, window, true)
}

fn window_pixels(
    ppm: &[u8],
    colour: [u8; 3],
    (width, height): (usize, usize),
    border: bool,
) -> Option<f64> {
    let head = &ppm[..ppm.len().min(32)];
    let text = String::from_utf8_lossy(head);
    let screen_width: usize = text.split_ascii_whitespace().nth(1)?.parse().ok()?;
    let all = scenario::ppm_pixels(ppm)?;
    if width == 0 || height == 0 || screen_width == 0 || all.len() % screen_width != 0 {
        return None;
    }
    let screen_height = all.len() / screen_width;
    if width > screen_width || height > screen_height {
        return None;
    }
    let (left, top) = ((screen_width - width) / 2, (screen_height - height) / 2);
    let near = |p: &[u8; 3]| {
        p.iter()
            .zip(colour)
            .all(|(&a, b)| a.abs_diff(b) <= TOLERANCE)
    };
    let on_border = |x: usize, y: usize| x == 0 || y == 0 || x == width - 1 || y == height - 1;
    let wanted: Vec<&[u8; 3]> = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .filter(|&(x, y)| !border || on_border(x, y))
        .map(|(x, y)| &all[(top + y) * screen_width + left + x])
        .collect();
    let shown = wanted.iter().filter(|p| near(p)).count();
    Some(shown as f64 / wanted.len() as f64)
}

/// The pixel at `(x, y)` of the screen dump `ppm`.
fn pixel(ppm: &[u8], (x, y): (usize, usize)) -> Option<[u8; 3]> {
    let head = &ppm[..ppm.len().min(32)];
    let text = String::from_utf8_lossy(head);
    let width: usize = text.split_ascii_whitespace().nth(1)?.parse().ok()?;
    scenario::ppm_pixels(ppm)?.get(y * width + x).copied()
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
        self.until_below(qemu, colour, 1.0, wanted, what)
    }

    /// As `until`, for the share of `colour` on the last `part` of the
    /// screen, its bottom rows.
    fn until_below(
        &mut self,
        qemu: &mut std::process::Child,
        colour: [u8; 3],
        part: f64,
        wanted: &dyn Fn(f64) -> bool,
        what: &str,
    ) -> Result<()> {
        loop {
            let ppm = scenario::screen_dump(self.socket, self.dump)?;
            let share =
                colour_share(&ppm, colour, part).context("the screen dump is not a PPM image")?;
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

impl Screen<'_> {
    /// Dumps the screen until the pixel at `at` is black, which `what`
    /// names, while `qemu` runs.
    fn until_black(
        &mut self,
        qemu: &mut std::process::Child,
        at: (usize, usize),
        what: &str,
    ) -> Result<()> {
        loop {
            let ppm = scenario::screen_dump(self.socket, self.dump)?;
            let shown = pixel(&ppm, at);
            if shown == Some([0, 0, 0]) {
                return Ok(());
            }
            if qemu.try_wait()?.is_some() {
                bail!("QEMU exited before {what}");
            }
            if Instant::now() > self.deadline {
                bail!("timed out before {what}, with {shown:?} at {at:?}");
            }
            std::thread::sleep(DUMP_EVERY);
        }
    }

    /// Dumps the screen until the `window` in the middle of the screen shows
    /// `colour` all over, which `what` names, while `qemu` runs,
    /// and fails at once if `log` says that the part that shows it ended,
    /// which `ended` is the line of.
    fn until_window(
        &mut self,
        qemu: &mut std::process::Child,
        log: &Path,
        ended: &str,
        colour: [u8; 3],
        window: (usize, usize),
        what: &str,
    ) -> Result<()> {
        loop {
            let ppm = scenario::screen_dump(self.socket, self.dump)?;
            let share = window_share(&ppm, colour, window)
                .context("the screen dump is not a PPM image with that window")?;
            let border = window_border_share(&ppm, colour, window).unwrap_or(0.0);
            // The client clears all of its window, so a pixel of another
            // colour is a fault, except for the pointer's arrow, which
            // starts in the middle of the screen, over the window.
            if share >= WINDOW_SHARE && border >= 1.0 {
                return Ok(());
            }
            let serial = fs::read_to_string(log).unwrap_or_default();
            if let Some(line) = serial.lines().find(|l| l.contains(ended)) {
                bail!("before {what}: {}", line.trim_end());
            }
            if qemu.try_wait()?.is_some() {
                bail!("QEMU exited before {what}");
            }
            if Instant::now() > self.deadline {
                bail!(
                    "timed out before {what}, with the colour on {:.0}% of the window",
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
    run_with(machine, kind, dir, timeout, &built, system, &SDL)
}

/// Boots the desktop run's scenario of `built` with the system `system`,
/// whose shell probe's switcher part shows `window`.
pub fn run_with(
    machine: &MachineArgs,
    kind: Kind,
    dir: &Path,
    timeout: Duration,
    built: &build::Built,
    system: &Path,
    window: &Window,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    let disk = dir.join("disk.img");
    image::create_with_system(kind, built, system, &disk, "")?;
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
        scenario::monitor(&socket, "mouse_button 1")?;
        scenario::monitor(&socket, "mouse_button 0")?;
        scenario::monitor(&socket, "sendkey k")?;
        screen.until(&mut qemu, TYPED, &covered, "the greeter took the key K")?;
        scenario::monitor(&socket, "sendkey ctrl-alt-f2")?;
        screen.until(&mut qemu, TYPED, &gone, "the console showed again")?;
        crate::console::type_keys(&socket, "d e s k t o p ret")?;
        scenario::wait_for_line(&log, DESKTOP_STARTED, &mut qemu, deadline)?;
        let what = "dee's desktop showed";
        screen.until(&mut qemu, WALLPAPER, &covered, what)?;
        let what = "the taskbar's haze showed";
        screen.until_below(&mut qemu, HAZE, HAZE_PART, &covered, what)?;
        let Window { colour, size, what } = *window;
        screen.until_window(&mut qemu, &log, SDL_EXITED, colour, size, what)?;
        let shown = pixel(&fs::read(&dump)?, ARROW_PIXEL);
        if shown != Some([0, 0, 0]) {
            bail!("the pointer's arrow is not in the middle of the screen: {shown:?}");
        }
        scenario::monitor(&socket, MOVE_TO_WALLPAPER)?;
        let what = "the arrow followed the mouse";
        screen.until_black(&mut qemu, MOVED_ARROW_PIXEL, what)?;
        // Where the arrow was, the frame drew the wallpaper again.
        let left = pixel(&fs::read(&dump)?, ARROW_PIXEL);
        if left == Some([0, 0, 0]) {
            bail!("the arrow stayed where it was before it moved");
        }
        scenario::monitor(&socket, MOVE_TO_PANEL)?;
        let what = "the panel's hand showed";
        screen.until_black(&mut qemu, HAND_PIXEL, what)?;
        scenario::wait_for_line(&log, CANCELLED, &mut qemu, deadline)?;
        let serial = scenario::wait_for_line(&log, SHELL_EXITED, &mut qemu, deadline)?;
        let exited = serial
            .lines()
            .find(|l| l.contains(SHELL_EXITED))
            .unwrap_or("");
        if !exited.trim_end().ends_with(SHELL_EXITED) {
            bail!("the shell probe was not refused: {}", exited.trim_end());
        }
        scenario::wait_for_line(&log, SHELL_RESTARTED, &mut qemu, deadline)?;
        scenario::wait_for_line(&log, SHELL_GAVE_UP, &mut qemu, deadline)?;
        scenario::monitor(&socket, "sendkey ctrl-alt-f2")?;
        let what = "the console showed from dee's desktop";
        screen.until(&mut qemu, WALLPAPER, &gone, what)?;
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
        let screen = ppm(&[
            [200, 40, 120],
            [201, 39, 122],
            [40, 90, 160],
            [200, 40, 120],
        ]);
        assert_eq!(colour_share(&screen, COLOUR, 1.0), Some(0.75));
        assert_eq!(colour_share(&screen, WALLPAPER, 1.0), Some(0.25));
        assert_eq!(colour_share(b"not a picture", COLOUR, 1.0), None);
        assert_eq!(colour_share(&screen, COLOUR, 0.5), Some(0.5));
        assert_eq!(pixel(&screen, (2, 0)), Some([40, 90, 160]));
        assert_eq!(pixel(&screen, (4, 0)), None);
    }

    #[test]
    fn counts_the_pixels_of_the_window_in_the_middle() {
        let (c, w) = (SDL.colour, WALLPAPER);
        // A screen of 6 by 4 pixels, whose 2 by 2 middle, from (2, 1),
        // has one pixel of the wrong colour, and a window colour elsewhere
        // that does not count.
        let rows = [
            [c, w, w, w, w, c],
            [c, w, c, c, w, c],
            [c, w, c, w, w, c],
            [c, w, w, w, w, c],
        ];
        let mut screen = b"P6\n6 4\n255\n".to_vec();
        screen.extend(rows.iter().flatten().flatten());
        assert_eq!(window_share(&screen, SDL.colour, (2, 2)), Some(0.75));
        assert_eq!(window_share(&screen, WALLPAPER, (2, 2)), Some(0.25));
        // Its border has three pixels of the right colour of four.
        assert_eq!(window_border_share(&screen, SDL.colour, (2, 2)), Some(0.75));
        assert_eq!(window_share(&screen, SDL.colour, (7, 1)), None);
        assert_eq!(window_share(&screen, SDL.colour, (2, 5)), None);
        assert_eq!(window_share(&screen, SDL.colour, (0, 2)), None);
        assert_eq!(window_share(b"not a picture", SDL.colour, (1, 1)), None);
    }
}
