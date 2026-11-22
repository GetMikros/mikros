//! Resizing windows with the pointer: by dragging the band around a frame
//! with the primary button, by dragging with the secondary button while Super
//! is held, which moves the nearest corner, and through
//! `xdg_toplevel.resize` (MIKSPEC-0074.R9). The frame follows the pointer at
//! every motion, with the client's content stretched to it until a buffer of
//! its size comes (MIKSPEC-0074.R15), and the client is asked for the size
//! with at most one configure outstanding (MIKSPEC-0074.R16).

use super::grabs::{BTN_LEFT, band_edges, nearest_corner, resize_frame};
use super::{ClientId, Rect, Role, Server, SurfaceId};

/// A window that the pointer resizes: the frame it had, the pointer's
/// place, the edges that move and the button that holds the resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Resize {
    pub surface: SurfaceId,
    pub edges: u32,
    pub frame: Rect,
    pub pointer: (i32, i32),
    pub button: u32,
    /// The size that the client was last asked for.
    pub asked: (i32, i32),
}

/// How long a client has to answer a configure with a size, in
/// milliseconds, before the next is sent anyway.
const PATIENCE: u64 = 1000;

/// The values of xdg_toplevel's resize edges.
const EDGES: [u32; 9] = [0, 1, 2, 4, 5, 6, 8, 9, 10];

/// Whether `edges` is a value that xdg_toplevel names.
pub(super) fn valid_edges(edges: u32) -> bool {
    EDGES.contains(&edges)
}

impl Server {
    /// Starts resizing the window of `surface` by `edges` with the pointer,
    /// which resizes it until `button` is released. Its client gets `leave`.
    pub(super) fn start_resize(&mut self, surface: SurfaceId, edges: u32, button: u32) {
        self.end_grabs();
        let Some(window) = self.windows().into_iter().find(|w| w.surface == surface) else {
            return;
        };
        // Fullscreen and maximized windows are not resized by hand.
        let fixed = self
            .surfaces
            .get(&surface)
            .and_then(|s| s.xdg.as_ref())
            .is_some_and(|x| x.fullscreen || x.maximized);
        if edges == 0 || fixed {
            return;
        }
        // Resizing ends a snap, which the frame before it is not kept for.
        self.with_window(surface, |w| {
            if w.snapped.take().is_some() {
                w.restore = None;
            }
        });
        self.pointer.resizing = Some(Resize {
            surface,
            edges,
            frame: window.frame,
            pointer: (self.pointer.x as i32, self.pointer.y as i32),
            button,
            asked: (window.frame.width, window.frame.height),
        });
        if let Some(xdg) = self.surfaces.get_mut(&surface).and_then(|s| s.xdg.as_mut()) {
            xdg.resizing = true;
        }
        self.leave_pointer_focus();
    }

    /// Gives the window that is resized the frame that the pointer's place
    /// makes, and asks its client for the size, and says whether a window is
    /// resized.
    pub(super) fn resize_with_pointer(&mut self) -> bool {
        let Some(resize) = self.pointer.resizing else {
            return false;
        };
        let mapped = self.windows().iter().any(|w| w.surface == resize.surface);
        let Some(surface) = self.surfaces.get(&resize.surface).filter(|_| mapped) else {
            self.end_grabs();
            return false;
        };
        let (min, max) = surface
            .xdg
            .as_ref()
            .map_or(((0, 0), (0, 0)), |x| (x.min_size, x.max_size));
        let delta = (
            self.pointer.x as i32 - resize.pointer.0,
            self.pointer.y as i32 - resize.pointer.1,
        );
        let mut frame = resize_frame(resize.frame, resize.edges, delta, min, max);
        // The top edge stays in the work area (MIKSPEC-0074.R8).
        let top = self.work_area().y;
        if frame.y < top {
            let cut = i64::from(top) - i64::from(frame.y);
            frame.height = (i64::from(frame.height) - cut).clamp(1, i64::from(i32::MAX)) as i32;
            frame.y = top;
        }
        self.set_frame(resize.surface, frame);
        let size = (frame.width, frame.height);
        if size != resize.asked {
            if let Some(r) = self.pointer.resizing.as_mut() {
                r.asked = size;
            }
            self.ask_size(resize.surface, size);
        }
        true
    }

    /// Ends the resize of a window when `button` is the button that holds
    /// it, tells the client its last size without the resizing state, and
    /// says whether it did.
    pub(super) fn end_resize(&mut self, button: u32) -> bool {
        let Some(resize) = self.pointer.resizing.filter(|r| r.button == button) else {
            return false;
        };
        self.pointer.resizing = None;
        if let Some(xdg) = self
            .surfaces
            .get_mut(&resize.surface)
            .and_then(|s| s.xdg.as_mut())
        {
            xdg.resizing = false;
        }
        if let Some(window) = self
            .windows()
            .into_iter()
            .find(|w| w.surface == resize.surface)
        {
            self.ask_size(resize.surface, (window.frame.width, window.frame.height));
        }
        true
    }

    /// `xdg_toplevel.resize` of the toplevel `surface` of `client`, after
    /// the press with `serial`, by `edges`.
    pub(super) fn request_resize(
        &mut self,
        client: ClientId,
        surface: SurfaceId,
        serial: u32,
        edges: u32,
    ) {
        if self.grabbing() {
            return;
        }
        let pressed = self.activation.got_press(client, serial);
        let button = self.button_held();
        let in_window = self.pointer_in(surface);
        let toplevel = self.surfaces.get(&surface).map(|s| s.role) == Some(Role::Toplevel);
        if let (true, Some(button), true, true) = (pressed, button, in_window, toplevel) {
            self.start_resize(surface, edges, button);
        }
    }

    /// The window whose band around its frame holds `(x, y)`, and the edges
    /// there, when no surface has the pointer and no window is over it.
    pub(super) fn band_at(&self, x: i32, y: i32) -> Option<(SurfaceId, u32)> {
        if self.pointer_has_surface() {
            return None;
        }
        for window in self.windows().into_iter().rev() {
            if window.minimized {
                continue;
            }
            let f = window.frame;
            if x >= f.x
                && y >= f.y
                && i64::from(x) < i64::from(f.x) + i64::from(f.width)
                && i64::from(y) < i64::from(f.y) + i64::from(f.height)
            {
                return None;
            }
            if let Some(edges) = band_edges(f, (x, y)) {
                return Some((window.surface, edges));
            }
        }
        None
    }

    /// Starts a resize for a press of `button` at the pointer, if it is one
    /// of the presses that start one, and says whether it did: the primary
    /// button in the band around a window, and the secondary button over a
    /// window while Super is held.
    pub(super) fn press_resizes(&mut self, button: u32) -> bool {
        // One grab at a time.
        if self.pointer.moving.is_some() || self.pointer.resizing.is_some() {
            return false;
        }
        let at = (self.pointer.x as i32, self.pointer.y as i32);
        if button == BTN_LEFT
            && let Some((surface, edges)) = self.band_at(at.0, at.1)
        {
            // A press in the band raises the window, and leaves the focus.
            self.raise(surface);
            self.start_resize(surface, edges, button);
            return true;
        }
        if button == super::grabs::BTN_RIGHT
            && self.super_held()
            && let Some(surface) = self.toplevel_under_pointer()
            && let Some(window) = self.windows().into_iter().find(|w| w.surface == surface)
        {
            self.raise(surface);
            let edges = nearest_corner(window.frame, at);
            self.start_resize(surface, edges, button);
            return true;
        }
        false
    }

    /// Asks the client of the window `surface` for the size `size` with
    /// `configure`, or, while a configure with a size is outstanding, keeps
    /// the latest size asked to send when the client has answered with a
    /// commit (MIKSPEC-0074.R16).
    pub(super) fn ask_size(&mut self, surface: SurfaceId, size: (i32, i32)) {
        let now = self.clock();
        let Some(s) = self.surfaces.get_mut(&surface) else {
            return;
        };
        let client = s.client;
        let Some(xdg) = s.xdg.as_mut() else {
            return;
        };
        // A client that has not answered within a second is not waited for.
        if xdg.outstanding.is_some() && now.saturating_sub(xdg.outstanding_at) >= PATIENCE {
            xdg.outstanding = None;
        }
        if xdg.outstanding.is_some() {
            xdg.latest = Some(size);
            return;
        }
        let serial = self.configure_with(client, surface, Some(size));
        if let Some(xdg) = self.surfaces.get_mut(&surface).and_then(|s| s.xdg.as_mut()) {
            xdg.outstanding = serial;
            xdg.outstanding_at = now;
            xdg.acked = false;
        }
    }

    /// The client of the toplevel `surface` committed: when it answered the
    /// outstanding configure and drew, it is asked for the latest size.
    pub(super) fn resize_committed(&mut self, surface: SurfaceId) {
        let Some(s) = self.surfaces.get_mut(&surface) else {
            return;
        };
        let drew = s.current.buffer.is_some();
        let Some(xdg) = s.xdg.as_mut() else {
            return;
        };
        if xdg.outstanding.is_none() || !xdg.acked || !drew {
            return;
        }
        xdg.outstanding = None;
        xdg.acked = false;
        if let Some(size) = xdg.latest.take() {
            self.ask_size(surface, size);
        }
    }
}
