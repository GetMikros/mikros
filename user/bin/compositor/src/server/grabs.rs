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
pub(super) const BTN_RIGHT: u32 = 0x111;

/// How much of a window's top edge stays inside the work area.
const KEPT: i32 = 32;

/// The edges of a window that a resize moves, as xdg_toplevel's resize
/// edges name them.
pub(super) const TOP: u32 = 1;
pub(super) const BOTTOM: u32 = 2;
pub(super) const LEFT: u32 = 4;
pub(super) const RIGHT: u32 = 8;

/// How far outside the edges of a frame, and how far along each edge from
/// a corner, a resize starts (MIKSPEC-0074.R9).
const BAND: i32 = 8;
const CORNER: i32 = 16;

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
        self.end_grabs();
        let Some(xdg) = self.surfaces.get(&surface).and_then(|s| s.xdg.as_ref()) else {
            return self.start_move_of_frame(surface, button);
        };
        // A fullscreen window does not move. A maximized one gets back the
        // frame it had, with the pointer at the same relative horizontal
        // place in it (MIKSPEC-0074.R27).
        if xdg.fullscreen {
            return;
        }
        let snapped = self
            .with_window(surface, |w| w.snapped.is_some())
            .unwrap_or(false);
        if xdg.maximized || snapped {
            self.restore_for_move(surface);
        }
        self.start_move_of_frame(surface, button);
    }

    fn start_move_of_frame(&mut self, surface: SurfaceId, button: u32) {
        let Some(window) = self.windows().into_iter().find(|w| w.surface == surface) else {
            return;
        };
        let offset = (
            (self.pointer.x as i32).saturating_sub(window.frame.x),
            (self.pointer.y as i32).saturating_sub(window.frame.y),
        );
        self.pointer.moving = Some(Move {
            surface,
            offset,
            button,
        });
        self.leave_pointer_focus();
    }

    /// Un-maximizes or un-snaps the window of `surface` and gives it back the frame that
    /// it had, moved so that the pointer is at the same relative horizontal
    /// place in it, and asks its client for that size.
    fn restore_for_move(&mut self, surface: SurfaceId) {
        let Some(window) = self.windows().into_iter().find(|w| w.surface == surface) else {
            return;
        };
        let Some(before) = self.take_restore(surface) else {
            return;
        };
        if let Some(xdg) = self.surfaces.get_mut(&surface).and_then(|s| s.xdg.as_mut()) {
            xdg.maximized = false;
        }
        self.with_window(surface, |w| w.snapped = None);
        let from = window.frame;
        let along = i64::from(self.pointer.x as i32) - i64::from(from.x);
        let x = i64::from(self.pointer.x as i32)
            - along * i64::from(before.width) / i64::from(from.width.max(1));
        let frame = Rect {
            x: x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            y: from.y,
            width: before.width,
            height: before.height,
        };
        self.set_frame(surface, frame);
        self.ask_size(surface, (frame.width, frame.height));
    }

    /// Ends the move or the resize that goes on, if one does.
    pub(super) fn end_grabs(&mut self) {
        self.pointer.moving = None;
        if let Some(resize) = self.pointer.resizing.take()
            && let Some(xdg) = self
                .surfaces
                .get_mut(&resize.surface)
                .and_then(|s| s.xdg.as_mut())
        {
            xdg.resizing = false;
        }
    }

    /// Whether a window is moved or resized.
    pub(super) fn grabbing(&self) -> bool {
        self.pointer.moving.is_some() || self.pointer.resizing.is_some()
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
        let Some(moving) = self.pointer.moving.filter(|m| m.button == button) else {
            return false;
        };
        self.pointer.moving = None;
        // A window dragged to the top edge of the output is maximized, to
        // the left or right edge snapped to that half (MIKSPEC-0074.R11).
        let width = self.screen.map_or(0, |(w, _)| w);
        let (x, y) = (self.pointer.x as i32, self.pointer.y as i32);
        if y <= 0 {
            self.maximize_window(moving.surface);
        } else if x <= 0 {
            self.snap_window(moving.surface, super::arrange::Snap::Left);
        } else if width > 0 && x >= width - 1 {
            self.snap_window(moving.surface, super::arrange::Snap::Right);
        }
        true
    }

    /// `xdg_toplevel.move` of the toplevel `surface` of `client`, after the
    /// press with `serial`: it starts the move when the client got that
    /// press, a button is held and the pointer is in that window.
    pub(super) fn request_move(&mut self, client: ClientId, surface: SurfaceId, serial: u32) {
        if self.grabbing() {
            return;
        }
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

/// The frame that a resize of `start` by `edges` gives when the pointer
/// moved by `(dx, dy)`, the size within `min` and `max` (0 for none) and
/// at least one pixel (MIKSPEC-0074.R9).
pub(super) fn resize_frame(
    start: Rect,
    edges: u32,
    (dx, dy): (i32, i32),
    min: (i32, i32),
    max: (i32, i32),
) -> Rect {
    let width = if edges & RIGHT != 0 {
        i64::from(start.width) + i64::from(dx)
    } else if edges & LEFT != 0 {
        i64::from(start.width) - i64::from(dx)
    } else {
        i64::from(start.width)
    };
    let height = if edges & BOTTOM != 0 {
        i64::from(start.height) + i64::from(dy)
    } else if edges & TOP != 0 {
        i64::from(start.height) - i64::from(dy)
    } else {
        i64::from(start.height)
    };
    let fit = |size: i64, min: i32, max: i32| {
        let upper = if max > 0 {
            i64::from(max)
        } else {
            i64::from(i32::MAX)
        };
        let lower = i64::from(min.max(1)).min(upper);
        size.clamp(lower, upper) as i32
    };
    let (width, height) = (fit(width, min.0, max.0), fit(height, min.1, max.1));
    // The edge that does not move stays where it was.
    let x = if edges & LEFT != 0 && edges & RIGHT == 0 {
        i64::from(start.x) + i64::from(start.width) - i64::from(width)
    } else {
        i64::from(start.x)
    };
    let y = if edges & TOP != 0 && edges & BOTTOM == 0 {
        i64::from(start.y) + i64::from(start.height) - i64::from(height)
    } else {
        i64::from(start.y)
    };
    Rect {
        x: x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        y: y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        width,
        height,
    }
}

/// The edges that the corner of `frame` nearest to `point` names, for a
/// resize with Super and the secondary button.
pub(super) fn nearest_corner(frame: Rect, (x, y): (i32, i32)) -> u32 {
    let horizontal = if i64::from(x) * 2 < 2 * i64::from(frame.x) + i64::from(frame.width) {
        LEFT
    } else {
        RIGHT
    };
    let vertical = if i64::from(y) * 2 < 2 * i64::from(frame.y) + i64::from(frame.height) {
        TOP
    } else {
        BOTTOM
    };
    horizontal | vertical
}

/// The edges that a press at `point` starts to resize, when it is in the
/// band just outside `frame`: 8 pixels wide, and a corner within 16 pixels
/// along an edge from the corner; None inside the frame or beyond the band.
pub(super) fn band_edges(frame: Rect, (x, y): (i32, i32)) -> Option<u32> {
    let (x, y) = (i64::from(x), i64::from(y));
    let (left, top) = (i64::from(frame.x), i64::from(frame.y));
    let (right, bottom) = (left + i64::from(frame.width), top + i64::from(frame.height));
    let band = i64::from(BAND);
    if x < left - band || x >= right + band || y < top - band || y >= bottom + band {
        return None;
    }
    if x >= left && x < right && y >= top && y < bottom {
        return None;
    }
    let mut edges = 0;
    if x < left {
        edges |= LEFT;
    } else if x >= right {
        edges |= RIGHT;
    }
    if y < top {
        edges |= TOP;
    } else if y >= bottom {
        edges |= BOTTOM;
    }
    // Along an edge, within 16 pixels of a corner it is the corner's.
    let corner = i64::from(CORNER);
    if edges & (LEFT | RIGHT) != 0 && edges & (TOP | BOTTOM) == 0 {
        if y < top + corner {
            edges |= TOP;
        } else if y >= bottom - corner {
            edges |= BOTTOM;
        }
    }
    if edges & (TOP | BOTTOM) != 0 && edges & (LEFT | RIGHT) == 0 {
        if x < left + corner {
            edges |= LEFT;
        } else if x >= right - corner {
            edges |= RIGHT;
        }
    }
    Some(edges)
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
    fn a_resize_moves_the_edges_it_names_within_the_sizes_of_the_client() {
        let start = rect(100, 100, 400, 300);
        let free = (0, 0);
        // The right and bottom edges follow the pointer.
        assert_eq!(
            resize_frame(start, RIGHT | BOTTOM, (50, -20), free, free),
            rect(100, 100, 450, 280)
        );
        // The left and top edges move the corner, the others stay.
        assert_eq!(
            resize_frame(start, LEFT | TOP, (30, 40), free, free),
            rect(130, 140, 370, 260)
        );
        // A side only changes one size.
        assert_eq!(
            resize_frame(start, RIGHT, (50, 999), free, free),
            rect(100, 100, 450, 300)
        );
        // The client's sizes bound it, and the fixed edge stays put.
        assert_eq!(
            resize_frame(start, LEFT, (-500, 0), free, (600, 0)),
            rect(-100, 100, 600, 300)
        );
        assert_eq!(
            resize_frame(start, LEFT, (390, 0), (100, 50), free),
            rect(400, 100, 100, 300)
        );
        // Never below one pixel, even beyond the opposite edge.
        assert_eq!(
            resize_frame(start, RIGHT, (-5000, 0), free, free),
            rect(100, 100, 1, 300)
        );
    }

    #[test]
    fn the_nearest_corner_is_chosen_by_the_halves_of_the_frame() {
        let frame = rect(0, 0, 100, 60);
        assert_eq!(nearest_corner(frame, (10, 10)), LEFT | TOP);
        assert_eq!(nearest_corner(frame, (90, 10)), RIGHT | TOP);
        assert_eq!(nearest_corner(frame, (10, 50)), LEFT | BOTTOM);
        assert_eq!(nearest_corner(frame, (90, 50)), RIGHT | BOTTOM);
    }

    #[test]
    fn a_resize_starts_in_the_band_around_the_frame() {
        let frame = rect(100, 100, 200, 150);
        assert_eq!(band_edges(frame, (95, 170)), Some(LEFT));
        assert_eq!(band_edges(frame, (300, 170)), Some(RIGHT));
        assert_eq!(band_edges(frame, (200, 92)), Some(TOP));
        assert_eq!(band_edges(frame, (200, 250)), Some(BOTTOM));
        // Within 16 pixels along an edge of a corner, it is the corner's.
        assert_eq!(band_edges(frame, (95, 110)), Some(LEFT | TOP));
        assert_eq!(band_edges(frame, (110, 95)), Some(LEFT | TOP));
        assert_eq!(band_edges(frame, (305, 240)), Some(RIGHT | BOTTOM));
        assert_eq!(band_edges(frame, (290, 255)), Some(RIGHT | BOTTOM));
        assert_eq!(band_edges(frame, (95, 95)), Some(LEFT | TOP));
        // Inside the frame, and beyond the band, it is not.
        assert_eq!(band_edges(frame, (150, 150)), None);
        assert_eq!(band_edges(frame, (91, 170)), None);
        assert_eq!(band_edges(frame, (200, 258)), None);
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
