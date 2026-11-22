//! Moving windows with the pointer (MIKSPEC-0074.R8): by dragging with the
//! primary button while Super is held, and through `xdg_toplevel.move`. While
//! a window is moved, the pointer moves it and its client gets no pointer
//! events, and the compositor keeps at least 32 pixels of the window's top
//! edge inside the work area.

use super::{ClientId, Role, Server, SurfaceId};
use crate::server::Rect;

/// The evdev codes of the Super keys and the primary button.
const KEY_LEFTMETA: u32 = 125;
const KEY_RIGHTMETA: u32 = 126;
pub(super) const BTN_LEFT: u32 = 0x110;

/// How much of a window's top edge stays inside the work area.
const KEPT: i32 = 32;

/// A window that the pointer moves, where the pointer holds it, and the
/// button whose release ends the move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Move {
    pub surface: SurfaceId,
    offset: (i32, i32),
    pub button: u32,
}

impl Server {
    /// Whether a Super key is held.
    pub(super) fn super_held(&self) -> bool {
        self.keyboard_pressed(KEY_LEFTMETA) || self.keyboard_pressed(KEY_RIGHTMETA)
    }

    /// Starts moving the window of `surface` with the pointer, which holds
    /// it where it is now, until `button` is released. The client of the
    /// window gets `leave`, and no more pointer events.
    pub(super) fn start_move(&mut self, surface: SurfaceId, button: u32) {
        let Some(window) = self.windows().into_iter().find(|w| w.surface == surface) else {
            return;
        };
        let offset = (
            self.pointer.x as i32 - window.frame.x,
            self.pointer.y as i32 - window.frame.y,
        );
        self.pointer.moving = Some(Move {
            surface,
            offset,
            button,
        });
        self.leave_pointer_focus();
    }

    /// Puts the window that moves where the pointer holds it, kept in the
    /// work area, and says whether a window moves.
    pub(super) fn move_with_pointer(&mut self) -> bool {
        let Some(moving) = self.pointer.moving else {
            return false;
        };
        let Some(window) = self
            .windows()
            .into_iter()
            .find(|w| w.surface == moving.surface)
        else {
            self.pointer.moving = None;
            return false;
        };
        let (x, y) = (
            self.pointer.x as i32 - moving.offset.0,
            self.pointer.y as i32 - moving.offset.1,
        );
        let (x, y) = keep_in(window.frame, (x, y), self.work_area());
        self.move_window(moving.surface, x, y);
        true
    }

    /// Ends the move of a window when `button` is the button that moved it,
    /// and says whether it did.
    pub(super) fn end_move(&mut self, button: u32) -> bool {
        if self.pointer.moving.is_some_and(|m| m.button == button) {
            self.pointer.moving = None;
            return true;
        }
        false
    }

    /// `xdg_toplevel.move` of the toplevel `surface` of `client`, after the
    /// press with `serial`: it starts the move when the client got that
    /// press, a button is held and the pointer is in that window.
    pub(super) fn request_move(&mut self, client: ClientId, surface: SurfaceId, serial: u32) {
        let pressed = self.activation.got_press(client, serial);
        let button = self.button_held();
        let in_window = self.pointer_in(surface);
        let toplevel = self.surfaces.get(&surface).map(|s| s.role) == Some(Role::Toplevel);
        if let (true, Some(button), true, true) = (pressed, button, in_window, toplevel) {
            self.start_move(surface, button);
        }
    }
}

/// Where a window of `frame`'s size that is to have its top-left corner at
/// `to` goes instead, so that at least 32 pixels of its top edge, or all of
/// it when it is narrower, stay in `area`.
fn keep_in(frame: Rect, to: (i32, i32), area: Rect) -> (i32, i32) {
    let kept = KEPT.min(frame.width.max(1));
    let low = i64::from(area.x) + i64::from(kept) - i64::from(frame.width);
    let high = i64::from(area.x) + i64::from(area.width) - i64::from(kept);
    let x = i64::from(to.0).clamp(low.min(high), high.max(low));
    let top = i64::from(area.y);
    let bottom = (i64::from(area.y) + i64::from(area.height) - 1).max(top);
    let y = i64::from(to.1).clamp(top, bottom);
    (x as i32, y as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn a_window_keeps_32_pixels_of_its_top_edge_in_the_work_area() {
        let area = rect(0, 32, 1280, 768);
        let frame = rect(10, 40, 400, 300);
        assert_eq!(keep_in(frame, (100, 200), area), (100, 200));
        // Too far left or right: 32 pixels remain.
        assert_eq!(keep_in(frame, (-1000, 200), area), (-368, 200));
        assert_eq!(keep_in(frame, (5000, 200), area), (1248, 200));
        // Not above the work area, and its top edge not below it.
        assert_eq!(keep_in(frame, (100, -50), area), (100, 32));
        assert_eq!(keep_in(frame, (100, 5000), area), (100, 799));
        // A window narrower than 32 pixels stays whole.
        let narrow = rect(0, 40, 20, 20);
        assert_eq!(keep_in(narrow, (-1000, 40), area), (0, 40));
        assert_eq!(keep_in(narrow, (5000, 40), area), (1260, 40));
    }
}
