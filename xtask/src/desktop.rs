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
/// What the session manager logs when the greeter, which the key L makes
/// log in `eve`, a user whose login mode is desktop, started eve's desktop,
/// and that it ended the greeter's compositor after the reply to Login
/// (MIKSPEC-0057.R42).
const EVE_STARTED: &str = "sessionmgr: started the desktop of eve for the greeter's login";
const GREETER_ENDED: &str = "sessionmgr: ended the compositor in greeter mode";
/// What the compositor logs once it cancelled the shell probe's
/// question, and once the shell probe exited, which it does with 0 when
/// it was refused.
const CANCELLED: &str = "compositor: cancelled the question";
const SHELL_EXITED: &str = "compositor: the part start-menu of system.mikros.shell_probe exited";
/// What the compositor logs when it starts that part again, as it does
/// each time the part exits, and when the part has exited too often
/// within a minute, which the probe, that exits at once, does.
/// What the compositor logs when the session manager forwards the
/// connection of an ordinary client, and what the run types to start one in
/// the user's text session: the greeter probe, which is the shell probe's
/// program, in its client mode, at the system apps' path.
const CLIENT_CONNECTED: &str = "compositor: a client of ";
const CLIENT_COMMAND: &str = "slash s y s t e m slash a p p s slash s y s t e m dot m i k r o s dot \
    s h e l l shift-minus p r o b e slash g r e e t e r minus p r o b e spc minus minus c l i e n t ret";
/// What the taskbar probe says of the window list: the first window has the
/// focus, and the click on the taskbar activated it.
const LIST_FOCUSED: &str = "greeter-probe: the window of system.mikros.shell_probe has the focus";
const LIST_ACTIVATED: &str = "greeter-probe: activated the window of system.mikros.shell_probe";
/// The pointer goes from the taskbar into the middle of the window, and
/// then, while Super is held, drags the window to the right by this much.
const MOVE_INTO_WINDOW: &str = "mouse_move -100 -377";
const DRAG: i32 = 100;
/// An ordinary client's window, which the run starts in the user's text
/// session, a rectangle of one colour that the compositor places in the
/// middle of the work area, and then resizes with the pointer at its
/// bottom right corner, by `GROW`: its colour then fills the frame, the
/// client having drawn no more.
const WINDOW_COMMAND: &str = "slash s y s t e m slash a p p s slash s y s t e m dot m i k r o s dot \
    s h e l l shift-minus p r o b e slash g r e e t e r minus p r o b e spc minus minus w i n d o w \
    spc shift-7 ret";
/// The protocols client (`--protocols` of the greeter probe), started the
/// same way: a toplevel of `PROTOCOLS_SIZE`, `PARENT` colour, with a
/// subsurface of 50 by 50 pixels at (10, 20), a cropped and scaled one
/// whose left half, 60 by 40 pixels, shows at (100, 30), and a popup of 40
/// by 30 pixels at (20, 120); and a layer surface of 24 pixels along the
/// left edge of the screen. It says when it is ready.
const PROTOCOLS_COMMAND: &str = "slash s y s t e m slash a p p s slash s y s t e m dot m i k r o s dot \
    s h e l l shift-minus p r o b e slash g r e e t e r minus p r o b e spc minus minus p r o t o c \
    o l s spc shift-7 ret";
const PROTOCOLS_WAITING: &str = "greeter-probe: protocols waiting for the keyboard";
const PROTOCOLS_READY: &str = "greeter-probe: protocols ready";
const PROTOCOLS_FAILED: &str = "greeter-probe: protocols failed";
const PROTOCOLS_SIZE: (usize, usize) = (240, 180);
const PROTOCOLS_PARENT: [u8; 3] = [60, 140, 220];
const PROTOCOLS_CHILD: [u8; 3] = [220, 60, 60];
const PROTOCOLS_VIEW_LEFT: [u8; 3] = [240, 200, 0];
const PROTOCOLS_VIEW_RIGHT: [u8; 3] = [0, 160, 160];
const PROTOCOLS_LAYER: [u8; 3] = [20, 200, 120];
const PROTOCOLS_POPUP: [u8; 3] = [250, 250, 60];
const APP_COLOUR: [u8; 3] = [120, 60, 200];
const APP_SIZE: (usize, usize) = (200, 150);
/// From where the pointer is after the drag to the band outside the app
/// window's bottom right corner.
const TO_APP_CORNER: &str = "mouse_move 2 79";
const GROW: (usize, usize) = (100, 60);
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
/// How long the run waits after a keyboard leave before it looks for the end
/// of the part that got it.
const KEYBOARD_LEAVE: Duration = Duration::from_secs(3);
const TOLERANCE: u8 = 2;
/// The share of the screen that the colour must cover, and the share
/// that it may still cover on a text console's screen.
const COVERED: f64 = 0.9;
const GONE: f64 = 0.1;
/// The area of the pointer's arrow, which starts with its hotspot in the
/// middle of the screen, over the window: from this many pixels left of and
/// above the hotspot to this many right of and below it, the last two
/// excluded. The arrow's pixels are blended with what is under them, so the
/// check leaves the area out and wants every other pixel of the window to
/// have the window's colour.
const ARROW_AREA: (isize, isize, isize, isize) = (-2, -2, 15, 20);
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
/// or None when it is not a PPM image or has no such rectangle. With
/// `arrow` the pixels in the pointer's arrow area, which has its hotspot
/// in the middle of the screen, do not count.
fn window_share(
    ppm: &[u8],
    colour: [u8; 3],
    (width, height): (usize, usize),
    arrow: bool,
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
    let (hotspot_x, hotspot_y) = ((screen_width / 2) as isize, (screen_height / 2) as isize);
    let (from_x, from_y, to_x, to_y) = ARROW_AREA;
    let in_arrow = |x: usize, y: usize| {
        let (dx, dy) = (x as isize - hotspot_x, y as isize - hotspot_y);
        arrow && (from_x..to_x).contains(&dx) && (from_y..to_y).contains(&dy)
    };
    let near = |p: &[u8; 3]| {
        p.iter()
            .zip(colour)
            .all(|(&a, b)| a.abs_diff(b) <= TOLERANCE)
    };
    let wanted: Vec<&[u8; 3]> = (0..height)
        .flat_map(|y| (0..width).map(move |x| (left + x, top + y)))
        .filter(|&(x, y)| !in_arrow(x, y))
        .map(|(x, y)| &all[y * screen_width + x])
        .collect();
    if wanted.is_empty() {
        return None;
    }
    let shown = wanted.iter().filter(|p| near(p)).count();
    Some(shown as f64 / wanted.len() as f64)
}

/// The smallest rectangle, as its left, top, right and bottom pixels, that
/// holds every pixel of `ppm` that shows `colour`, or None when none does
/// or it is not a PPM image.
fn colour_bounds(ppm: &[u8], colour: [u8; 3]) -> Option<(usize, usize, usize, usize)> {
    let head = &ppm[..ppm.len().min(32)];
    let text = String::from_utf8_lossy(head);
    let width: usize = text.split_ascii_whitespace().nth(1)?.parse().ok()?;
    let all = scenario::ppm_pixels(ppm)?;
    let near = |p: &[u8; 3]| {
        p.iter()
            .zip(colour)
            .all(|(&a, b)| a.abs_diff(b) <= TOLERANCE)
    };
    let mut bounds: Option<(usize, usize, usize, usize)> = None;
    for (i, _) in all.iter().enumerate().filter(|(_, p)| near(p)) {
        let (x, y) = (i % width, i / width);
        bounds = Some(match bounds {
            None => (x, y, x, y),
            Some((l, t, r, b)) => (l.min(x), t.min(y), r.max(x), b.max(y)),
        });
    }
    bounds
}

/// Whether the screen dump `ppm` shows what the protocols client draws, or
/// what is wrong with it: the toplevel, the subsurfaces and the popup in
/// their places and sizes, and the layer surface along the left edge.
fn protocols_shown(ppm: &[u8]) -> std::result::Result<(), String> {
    let bounds = |colour| colour_bounds(ppm, colour);
    let size = |(l, t, r, b): (usize, usize, usize, usize)| (r - l + 1, b - t + 1);
    let Some(parent) = bounds(PROTOCOLS_PARENT) else {
        return Err("the toplevel is not on the screen".into());
    };
    if size(parent) != PROTOCOLS_SIZE {
        return Err(format!("the toplevel is {:?}", size(parent)));
    }
    let at = |name: &str, colour, offset: (usize, usize), want: (usize, usize)| {
        let Some(found) = bounds(colour) else {
            return Err(format!("{name} is not on the screen"));
        };
        if size(found) != want {
            return Err(format!("{name} is {:?}, not {want:?}", size(found)));
        }
        let place = (
            found.0.wrapping_sub(parent.0),
            found.1.wrapping_sub(parent.1),
        );
        if place != offset {
            return Err(format!(
                "{name} lies at {place:?} of the toplevel, not {offset:?}"
            ));
        }
        Ok(())
    };
    at(
        "the synchronized subsurface",
        PROTOCOLS_CHILD,
        (10, 20),
        (50, 50),
    )?;
    at(
        "the cropped subsurface",
        PROTOCOLS_VIEW_LEFT,
        (100, 30),
        (60, 40),
    )?;
    at("the popup", PROTOCOLS_POPUP, (20, 120), (40, 30))?;
    if bounds(PROTOCOLS_VIEW_RIGHT).is_some() {
        return Err("the part of the buffer outside the crop is shown".into());
    }
    let Some(layer) = bounds(PROTOCOLS_LAYER) else {
        return Err("the layer surface is not on the screen".into());
    };
    if layer.0 != 0 || size(layer).0 != 24 || size(layer).1 < 600 {
        return Err(format!("the layer surface is {layer:?}"));
    }
    Ok(())
}

/// Waits until the serial log `log` has at least `count` lines with `text`.
fn wait_for_lines(
    log: &Path,
    text: &str,
    count: usize,
    qemu: &mut std::process::Child,
    deadline: Instant,
) -> Result<()> {
    loop {
        let serial = String::from_utf8_lossy(&fs::read(log).unwrap_or_default()).into_owned();
        if serial.lines().filter(|l| l.contains(text)).count() >= count {
            return Ok(());
        }
        if qemu.try_wait()?.is_some() {
            bail!("QEMU exited before {count} lines with {text:?}");
        }
        if Instant::now() > deadline {
            bail!("timed out waiting for {count} lines with {text:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The width of the screen dump `ppm`.
fn screen_width_of(ppm: &[u8]) -> Option<usize> {
    let head = &ppm[..ppm.len().min(32)];
    String::from_utf8_lossy(head)
        .split_ascii_whitespace()
        .nth(1)?
        .parse()
        .ok()
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

    /// Dumps the screen until the pixels of `colour` fill a rectangle of
    /// `size` whose left edge is within 2 pixels of `left`, and returns its
    /// top left corner.
    fn until_colour_at(
        &mut self,
        qemu: &mut std::process::Child,
        colour: [u8; 3],
        size: (usize, usize),
        left: usize,
        what: &str,
    ) -> Result<(usize, usize)> {
        loop {
            let ppm = scenario::screen_dump(self.socket, self.dump)?;
            if let Some((l, t, r, b)) = colour_bounds(&ppm, colour)
                && (r - l + 1, b - t + 1) == size
                && l.abs_diff(left) <= 2
            {
                return Ok((l, t));
            }
            if qemu.try_wait()?.is_some() {
                bail!("QEMU exited before {what}");
            }
            if Instant::now() > self.deadline {
                bail!("timed out before {what}");
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
            let share = window_share(&ppm, colour, window, true)
                .context("the screen dump is not a PPM image with that window")?;
            // The client draws all of its window in one colour, so a pixel
            // of another colour is a fault, except in the pointer's arrow,
            // which starts in the middle of the screen, over the window.
            if share >= 1.0 {
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
    run_with(machine, kind, dir, timeout, &built, system, &SDL, true)
}

/// Boots the desktop run's scenario of `built` with the system `system`,
/// whose shell probe's switcher part shows `window`, and with the login of
/// a desktop user at the greeter after dee's desktop when `desktop_login`.
#[allow(clippy::too_many_arguments)]
pub fn run_with(
    machine: &MachineArgs,
    kind: Kind,
    dir: &Path,
    timeout: Duration,
    built: &build::Built,
    system: &Path,
    window: &Window,
    desktop_login: bool,
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
        // The taskbar probe's window list tells that the window has the
        // focus, and a click on the taskbar activates it through the list.
        scenario::wait_for_line(&log, LIST_FOCUSED, &mut qemu, deadline)?;
        scenario::monitor(&socket, "mouse_button 1")?;
        scenario::monitor(&socket, "mouse_button 0")?;
        scenario::wait_for_line(&log, LIST_ACTIVATED, &mut qemu, deadline)?;
        // Super and the primary button drag the window, which moves by what
        // the pointer moved.
        scenario::monitor(&socket, MOVE_INTO_WINDOW)?;
        let left = (screen_width_of(&fs::read(&dump)?).unwrap_or(1280) - size.0) / 2;
        scenario::monitor(&socket, "sendkey meta_l 3000")?;
        std::thread::sleep(Duration::from_secs(1));
        scenario::monitor(&socket, "mouse_button 1")?;
        scenario::monitor(&socket, &format!("mouse_move {DRAG} 0"))?;
        scenario::monitor(&socket, "mouse_button 0")?;
        let what = "the window was dragged with Super";
        screen.until_colour_at(&mut qemu, colour, size, left + DRAG as usize, what)?;
        // Super is let go after its hold.
        std::thread::sleep(Duration::from_secs(4));
        // Super and Up maximize the window, which goes to the corner of the
        // work area, and Super and Down give it back where it was dragged.
        scenario::monitor(&socket, "sendkey meta_l-up")?;
        let what = "the window was maximized with Super and Up";
        screen.until_colour_at(&mut qemu, colour, size, 0, what)?;
        scenario::monitor(&socket, "sendkey meta_l-down")?;
        let what = "the window was restored with Super and Down";
        screen.until_colour_at(&mut qemu, colour, size, left + DRAG as usize, what)?;
        // Alt and Tab take the keyboard from the window, which is a keyboard
        // leave for its client, and give it back. A part that ends with it
        // is started again, so its end is in the log, not on the screen.
        scenario::monitor(&socket, "sendkey alt-tab 2000")?;
        std::thread::sleep(KEYBOARD_LEAVE);
        let serial = fs::read_to_string(&log).unwrap_or_default();
        if let Some(line) = serial.lines().find(|l| l.contains(SDL_EXITED)) {
            bail!(
                "the window's part ended at a keyboard leave: {}",
                line.trim_end()
            );
        }
        let what = "the window showed after Alt and Tab";
        screen.until_colour_at(&mut qemu, colour, size, left + DRAG as usize, what)?;
        // An ordinary client's window, from the text session.
        scenario::monitor(&socket, "sendkey ctrl-alt-f2")?;
        screen.until(
            &mut qemu,
            WALLPAPER,
            &gone,
            "the console showed from the desktop",
        )?;
        crate::console::type_keys(&socket, WINDOW_COMMAND)?;
        std::thread::sleep(Duration::from_secs(2));
        scenario::monitor(&socket, "sendkey ctrl-alt-f1")?;
        // The desktop shows again with the window that was dragged where it
        // was put.
        let what = "the desktop showed the dragged window again";
        screen.until_colour_at(&mut qemu, colour, size, left + DRAG as usize, what)?;
        let (app_left, _) = screen.until_colour_at(
            &mut qemu,
            APP_COLOUR,
            APP_SIZE,
            (screen_width_of(&fs::read(&dump)?).unwrap_or(1280) - APP_SIZE.0) / 2,
            "the client's window showed",
        )?;
        // The pointer drags the band at its bottom right corner.
        scenario::monitor(&socket, TO_APP_CORNER)?;
        scenario::monitor(&socket, "mouse_button 1")?;
        scenario::monitor(&socket, &format!("mouse_move {} {}", GROW.0, GROW.1))?;
        scenario::monitor(&socket, "mouse_button 0")?;
        let what = "the client's window was resized with the pointer";
        screen.until_colour_at(
            &mut qemu,
            APP_COLOUR,
            (APP_SIZE.0 + GROW.0, APP_SIZE.1 + GROW.1),
            app_left,
            what,
        )?;
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
        // The protocols client, which has the keyboard once the other two
        // windows are minimized, and the screen it draws.
        crate::console::type_keys(&socket, PROTOCOLS_COMMAND)?;
        std::thread::sleep(Duration::from_secs(3));
        scenario::monitor(&socket, "sendkey ctrl-alt-f1")?;
        let what = "the desktop showed for the protocols client";
        screen.until(&mut qemu, WALLPAPER, &|share: f64| share > GONE, what)?;
        // The client says that it waits for the keyboard once its shortcuts
        // inhibitor exists, so that a Super and Down that reaches it after it
        // got the keyboard is its own, not the compositor's.
        scenario::wait_for_line(&log, PROTOCOLS_WAITING, &mut qemu, deadline)?;
        for _ in 0..2 {
            scenario::monitor(&socket, "sendkey meta_l-down")?;
            std::thread::sleep(Duration::from_secs(1));
        }
        loop {
            let serial = fs::read_to_string(&log).unwrap_or_default();
            if let Some(line) = serial.lines().find(|l| l.contains(PROTOCOLS_FAILED)) {
                bail!("{}", line.trim_end());
            }
            if serial.contains(PROTOCOLS_READY) {
                break;
            }
            if qemu.try_wait()?.is_some() {
                bail!("QEMU exited before the protocols client was ready");
            }
            if Instant::now() > deadline {
                bail!("timed out waiting for the protocols client");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let ppm = scenario::screen_dump(&socket, &dump)?;
        if let Err(why) = protocols_shown(&ppm) {
            fs::write(dir.join("protocols.ppm"), &ppm)?;
            bail!("the protocols client's screen is wrong: {why}");
        }
        scenario::monitor(&socket, "sendkey ctrl-alt-f2")?;
        let what = "the console showed after the protocols client";
        screen.until(&mut qemu, WALLPAPER, &gone, what)?;
        crate::console::type_keys(&socket, CLIENT_COMMAND)?;
        // The console after the command, which `client.ppm` keeps.
        std::thread::sleep(Duration::from_secs(5));
        fs::write(
            dir.join("client.ppm"),
            scenario::screen_dump(&socket, &dump)?,
        )?;
        // The windows of the app and of the protocols client connected as
        // well, so the client of this step is the third connection.
        wait_for_lines(&log, CLIENT_CONNECTED, 3, &mut qemu, deadline)?;
        crate::console::type_keys(&socket, "e x i t ret")?;
        scenario::wait_for_line(&log, DESKTOP_ENDED, &mut qemu, deadline)?;
        if !desktop_login {
            return Ok(());
        }
        // The greeter started again and logged dee in on the console again,
        // as its arguments say; the desktop's screen shows the greeter,
        // and the key L logs `eve` in there, whose desktop then owns the
        // seat while the greeter's compositor ends.
        wait_for_lines(&log, LOGGED_IN, 2, &mut qemu, deadline)?;
        screen.until(
            &mut qemu,
            COLOUR,
            &gone,
            "the console showed after the greeter",
        )?;
        scenario::monitor(&socket, "sendkey ctrl-alt-f1")?;
        screen.until(&mut qemu, COLOUR, &covered, "the greeter showed again")?;
        // The click gives the greeter the keyboard, as in the first step.
        scenario::monitor(&socket, "mouse_button 1")?;
        scenario::monitor(&socket, "mouse_button 0")?;
        scenario::monitor(&socket, "sendkey l")?;
        scenario::wait_for_line(&log, EVE_STARTED, &mut qemu, deadline)?;
        // The first of these lines is from the greeter that dee's desktop
        // ended before; the second is from this login.
        wait_for_lines(&log, GREETER_ENDED, 2, &mut qemu, deadline)?;
        screen.until(&mut qemu, WALLPAPER, &covered, "eve's desktop showed")
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

    /// A screen of 1280 by 800 pixels of the wallpaper's colour, with the
    /// rectangles `(left, top, width, height, colour)` drawn in order.
    fn screen_with(rects: &[(usize, usize, usize, usize, [u8; 3])]) -> Vec<u8> {
        let mut pixels = vec![WALLPAPER; 1280 * 800];
        for &(left, top, width, height, colour) in rects {
            for y in top..top + height {
                for x in left..left + width {
                    pixels[y * 1280 + x] = colour;
                }
            }
        }
        let mut ppm = b"P6\n1280 800\n255\n".to_vec();
        ppm.extend(pixels.iter().flatten());
        ppm
    }

    /// What the protocols client draws, with its toplevel at (500, 300).
    fn protocols_screen(wrong: Option<&str>) -> Vec<u8> {
        let (l, t) = (500, 300);
        let mut rects = vec![
            (0, 0, 24, 800, PROTOCOLS_LAYER),
            (l, t, 240, 180, PROTOCOLS_PARENT),
            (l + 10, t + 20, 50, 50, PROTOCOLS_CHILD),
            (l + 100, t + 30, 60, 40, PROTOCOLS_VIEW_LEFT),
            (l + 20, t + 120, 40, 30, PROTOCOLS_POPUP),
        ];
        match wrong {
            Some("uncropped") => rects.push((l + 130, t + 30, 30, 40, PROTOCOLS_VIEW_RIGHT)),
            Some("moved") => rects[2] = (l + 12, t + 20, 50, 50, PROTOCOLS_CHILD),
            Some("narrow layer") => rects[0] = (0, 0, 20, 800, PROTOCOLS_LAYER),
            Some("no popup") => {
                rects.remove(4);
            }
            _ => {}
        }
        screen_with(&rects)
    }

    #[test]
    fn checks_what_the_protocols_client_draws() {
        assert_eq!(protocols_shown(&protocols_screen(None)), Ok(()));
        for wrong in ["uncropped", "moved", "narrow layer", "no popup"] {
            assert!(
                protocols_shown(&protocols_screen(Some(wrong))).is_err(),
                "{wrong}"
            );
        }
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
    fn finds_the_rectangle_of_a_colour() {
        let (c, w) = (SDL.colour, WALLPAPER);
        let rows = [[w, w, w, w], [w, c, c, w], [w, c, c, w], [w, w, w, w]];
        let mut screen = b"P6\n4 4\n255\n".to_vec();
        screen.extend(rows.iter().flatten().flatten());
        assert_eq!(colour_bounds(&screen, c), Some((1, 1, 2, 2)));
        assert_eq!(colour_bounds(&screen, [1, 2, 3]), None);
        assert_eq!(colour_bounds(b"not a picture", c), None);
        assert_eq!(screen_width_of(&screen), Some(4));
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
        assert_eq!(window_share(&screen, SDL.colour, (2, 2), false), Some(0.75));
        assert_eq!(window_share(&screen, WALLPAPER, (2, 2), false), Some(0.25));
        assert_eq!(window_share(&screen, SDL.colour, (7, 1), false), None);
        assert_eq!(window_share(&screen, SDL.colour, (2, 5), false), None);
        assert_eq!(window_share(&screen, SDL.colour, (0, 2), false), None);
        assert_eq!(
            window_share(b"not a picture", SDL.colour, (1, 1), false),
            None
        );
    }

    #[test]
    fn leaves_the_pointers_arrow_out_of_the_window() {
        let (c, w) = (SDL.colour, WALLPAPER);
        // A screen of 40 by 30 pixels, whose middle, the arrow's hotspot, is
        // (20, 15), and a window of 10 by 6 from (15, 12) in one colour.
        let mut screen = b"P6\n40 30\n255\n".to_vec();
        let mut pixels = vec![w; 40 * 30];
        for y in 12..18 {
            for x in 15..25 {
                pixels[y * 40 + x] = c;
            }
        }
        let write = |pixels: &[[u8; 3]], screen: &mut Vec<u8>| {
            screen.truncate(b"P6\n40 30\n255\n".len());
            screen.extend(pixels.iter().flatten());
        };
        write(&pixels, &mut screen);
        assert_eq!(window_share(&screen, c, (10, 6), true), Some(1.0));
        // A wrong pixel in the arrow's area does not count, and a wrong
        // pixel outside it, next to the hotspot's side of the window, does.
        pixels[15 * 40 + 20] = [0, 0, 0];
        write(&pixels, &mut screen);
        assert_eq!(window_share(&screen, c, (10, 6), true), Some(1.0));
        assert_eq!(window_share(&screen, c, (10, 6), false), Some(59.0 / 60.0));
        pixels[12 * 40 + 16] = [0, 0, 0];
        write(&pixels, &mut screen);
        assert!(window_share(&screen, c, (10, 6), true).unwrap() < 1.0);
    }
}
