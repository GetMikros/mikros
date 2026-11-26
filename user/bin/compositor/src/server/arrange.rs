//! Maximizing, snapping, restoring and minimizing windows with the
//! compositor's own commands (MIKSPEC-0074.R27, MIKSPEC-0074.R11): the
//! shortcuts of MIKSPEC-0074.R13 and dragging a window to an edge of the
//! output use them. A maximized or snapped window gives back the frame it had
//! before when it is restored.

use super::{Rect, Server, SurfaceId};

/// The evdev codes of the keys of the shortcuts (MIKSPEC-0074.R13).
const KEY_ESC: u32 = 1;
const KEY_TAB: u32 = 15;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_RIGHTSHIFT: u32 = 54;
const KEY_LEFTMETA: u32 = 125;
const KEY_RIGHTMETA: u32 = 126;
const KEY_F4: u32 = 62;
const KEY_UP: u32 = 103;
const KEY_LEFT: u32 = 105;
const KEY_RIGHT: u32 = 106;
const KEY_DOWN: u32 = 108;
const KEY_LEFTCTRL: u32 = 29;
const KEY_RIGHTCTRL: u32 = 97;
const KEY_LEFTALT: u32 = 56;
const KEY_RIGHTALT: u32 = 100;

/// The half of the work area that a window is snapped to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Snap {
    Left,
    Right,
}

impl Server {
    /// Maximizes the window of `surface`: its client is asked for the work
    /// area's size, and the frame it had is kept to restore. A fullscreen
    /// window stays so.
    pub fn maximize_window(&mut self, surface: SurfaceId) {
        let Some(xdg) = self.surfaces.get_mut(&surface).and_then(|s| s.xdg.as_mut()) else {
            return;
        };
        if xdg.fullscreen || xdg.maximized {
            return;
        }
        xdg.maximized = true;
        // A snap is over; the frame to restore is the one before it, or, when
        // the window was not snapped, the one that the window layer keeps
        // when it sees the window maximized.
        self.with_window(surface, |w| w.snapped = None);
        self.configure_now(surface, None);
    }

    /// Gives a maximized or snapped window the frame it had before, and
    /// says whether the window was one.
    pub fn restore_window(&mut self, surface: SurfaceId) -> bool {
        let maximized = self
            .surfaces
            .get(&surface)
            .and_then(|s| s.xdg.as_ref())
            .is_some_and(|x| x.maximized);
        let snapped = self
            .with_window(surface, |w| w.snapped.is_some())
            .unwrap_or(false);
        if !maximized && !snapped {
            return false;
        }
        let Some(before) = self.take_restore(surface) else {
            return false;
        };
        if let Some(xdg) = self.surfaces.get_mut(&surface).and_then(|s| s.xdg.as_mut()) {
            xdg.maximized = false;
        }
        self.with_window(surface, |w| w.snapped = None);
        self.set_frame(surface, before);
        // The configure drops the maximized state: it goes out at once
        // (MIKSPEC-0074.R30).
        if maximized {
            self.configure_now(surface, Some((before.width, before.height)));
        } else {
            self.ask_size(surface, (before.width, before.height));
        }
        true
    }

    /// Snaps the window of `surface` to the left or right half of the work
    /// area, which keeps the frame it had to restore.
    pub fn snap_window(&mut self, surface: SurfaceId, side: Snap) {
        let area = self.work_area();
        let half = area.width / 2;
        let frame = match side {
            Snap::Left => Rect {
                x: area.x,
                y: area.y,
                width: half,
                height: area.height,
            },
            Snap::Right => Rect {
                x: area.x + half,
                y: area.y,
                width: area.width - half,
                height: area.height,
            },
        };
        let Some(xdg) = self.surfaces.get_mut(&surface).and_then(|s| s.xdg.as_mut()) else {
            return;
        };
        if xdg.fullscreen || frame.width < 1 || frame.height < 1 {
            return;
        }
        let was_maximized = std::mem::replace(&mut xdg.maximized, false);
        // What it had before it was maximized or snapped is what it goes
        // back to.
        let kept = self.with_window(surface, |w| {
            if was_maximized || w.snapped.is_some() {
                w.restore
            } else {
                Some(w.frame)
            }
        });
        let Some(kept) = kept.flatten() else {
            return;
        };
        self.with_window(surface, |w| {
            w.restore = Some(kept);
            w.snapped = Some(side);
        });
        self.set_frame(surface, frame);
        // A configure that drops the maximized state goes out at once
        // (MIKSPEC-0074.R30).
        if was_maximized {
            self.configure_now(surface, Some((frame.width, frame.height)));
        } else {
            self.ask_size(surface, (frame.width, frame.height));
        }
    }

    /// Minimizes the window of `surface`.
    pub fn minimize_window(&mut self, surface: SurfaceId) {
        if let Some(xdg) = self.surfaces.get_mut(&surface).and_then(|s| s.xdg.as_mut()) {
            xdg.minimized = true;
        }
        // The keyboard moves on at once, not with the next pointer event.
        self.keep_keyboard_focus();
    }

    /// Whether an Alt key is held.
    fn alt_held(&self) -> bool {
        self.keyboard_pressed(KEY_LEFTALT) || self.keyboard_pressed(KEY_RIGHTALT)
    }

    /// Handles the press or release of `key` as a shortcut of the compositor
    /// (MIKSPEC-0074.R13), and says whether it was one, which no client
    /// gets: Super and Up maximize the focused window, Super and Down restore
    /// it when it is maximized or snapped and minimize it otherwise, Super
    /// and Left or Right snap it, and Alt and F4 ask it to close.
    pub(super) fn shortcut(&mut self, key: u32, pressed: bool) -> bool {
        // A client that inhibited them has the keys, and the release of a
        // key whose press was a shortcut is still not its own.
        if self.shortcuts_inhibited() {
            return !pressed && self.keyboard.swallowed.remove(&key);
        }
        // Alt let go ends the window switcher with its selection.
        if !pressed
            && (key == KEY_LEFTALT || key == KEY_RIGHTALT)
            && self.switching.is_open()
            && !self.alt_held()
        {
            self.end_switcher(true);
        }
        // A Super key alone asks for the start menu.
        if key == KEY_LEFTMETA || key == KEY_RIGHTMETA {
            self.super_pressed(pressed);
            return false;
        }
        if pressed {
            self.other_key_pressed();
        }
        if pressed && self.switching.is_open() && key == KEY_ESC {
            self.end_switcher(false);
            self.keyboard.swallowed.insert(key);
            return true;
        }
        if pressed
            && key == KEY_TAB
            && self.alt_held()
            && !self.keyboard_pressed(KEY_LEFTCTRL)
            && !self.keyboard_pressed(KEY_RIGHTCTRL)
        {
            let backwards =
                self.keyboard_pressed(KEY_LEFTSHIFT) || self.keyboard_pressed(KEY_RIGHTSHIFT);
            self.switch_windows(backwards);
            self.keyboard.swallowed.insert(key);
            return true;
        }
        if !pressed {
            // The release of a key whose press was a shortcut is not a
            // client's either.
            return self.keyboard.swallowed.remove(&key);
        }
        let super_keys = self.super_held();
        let window = self.keyboard_focus();
        let handled = match key {
            KEY_UP | KEY_DOWN | KEY_LEFT | KEY_RIGHT if super_keys => {
                if let Some(surface) = window {
                    match key {
                        KEY_UP => self.maximize_window(surface),
                        KEY_DOWN => {
                            if !self.restore_window(surface) {
                                self.minimize_window(surface);
                            }
                        }
                        KEY_LEFT => self.snap_window(surface, Snap::Left),
                        _ => self.snap_window(surface, Snap::Right),
                    }
                }
                true
            }
            // With Ctrl too it is the switch to another screen, not for windows.
            KEY_F4
                if self.alt_held()
                    && !self.keyboard_pressed(KEY_LEFTCTRL)
                    && !self.keyboard_pressed(KEY_RIGHTCTRL) =>
            {
                if let Some(surface) = window {
                    self.close_window(surface);
                }
                true
            }
            _ => false,
        };
        if handled {
            self.keyboard.swallowed.insert(key);
        }
        handled
    }
}
