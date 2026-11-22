//! The window layer: every mapped toplevel is a window with a frame, a
//! rectangle in logical pixels in the space that the outputs share, and
//! the windows lie in one stacking order, bottom first (MIKSPEC-0074.R1).
//! A window is mapped when its toplevel has a buffer, and takes the
//! buffer's size; a new one is placed in the work area of the output
//! (MIKSPEC-0074.R2, MIKSPEC-0074.R4). There is one output, at the
//! space's origin.

use super::panel::Edge;
use super::{Rect, Role, Server, SurfaceId};
use std::cell::RefCell;

/// A new window moves right and down by this much while its top-left
/// corner is within `NEAR` of another window's.
const CASCADE: i32 = 32;
const NEAR: i32 = 8;

/// A mapped toplevel and its frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub surface: SurfaceId,
    pub frame: Rect,
}

/// The windows, bottom first.
#[derive(Debug, Default)]
pub(super) struct Windows {
    stack: RefCell<Vec<Window>>,
}

impl Server {
    /// The work area of the output: its rectangle minus the exclusive
    /// zones of its configured panels (MIKSPEC-0074.R2).
    pub fn work_area(&self) -> Rect {
        let screen = self.screen.unwrap_or((0, 0));
        let mut area = Rect {
            x: 0,
            y: 0,
            width: screen.0,
            height: screen.1,
        };
        for surface in self.surfaces.values() {
            let Some(panel) = surface
                .panel
                .as_ref()
                .filter(|p| p.configured && p.exclusive)
            else {
                continue;
            };
            let zone = panel.place(screen);
            match panel.edge {
                Edge::Top => {
                    area.y += zone.height;
                    area.height -= zone.height;
                }
                Edge::Bottom => area.height -= zone.height,
                Edge::Left => {
                    area.x += zone.width;
                    area.width -= zone.width;
                }
                Edge::Right => area.width -= zone.width,
            }
        }
        area.width = area.width.max(0);
        area.height = area.height.max(0);
        area
    }

    /// The windows, bottom first, after mapping the toplevels that have a
    /// buffer now and forgetting those that have none or are gone.
    pub fn windows(&self) -> Vec<Window> {
        let mut stack = self.windows.stack.borrow_mut();
        let size = |buffer| self.buffer_sizes.get(&buffer).copied().unwrap_or((0, 0));
        let mapped: Vec<(SurfaceId, (i32, i32))> = self
            .surfaces
            .iter()
            .filter(|(_, s)| s.role == Role::Toplevel)
            .filter_map(|(&id, s)| {
                let (w, h) = size(s.current.buffer?);
                Some((id, (w as i32, h as i32)))
            })
            .collect();
        stack.retain_mut(|w| match mapped.iter().find(|(id, _)| *id == w.surface) {
            Some(&(_, (width, height))) => {
                w.frame.width = width;
                w.frame.height = height;
                true
            }
            None => false,
        });
        for &(id, (width, height)) in &mapped {
            if stack.iter().any(|w| w.surface == id) {
                continue;
            }
            let frame = self.place(&stack, width, height);
            stack.push(Window { surface: id, frame });
        }
        stack.clone()
    }

    /// Puts the frame of the window of `surface` with its top-left corner
    /// at `x`, `y`, when it is mapped.
    pub fn move_window(&self, surface: SurfaceId, x: i32, y: i32) {
        self.windows();
        if let Some(w) = self
            .windows
            .stack
            .borrow_mut()
            .iter_mut()
            .find(|w| w.surface == surface)
        {
            w.frame.x = x;
            w.frame.y = y;
        }
    }

    /// Where a new window of `width` by `height` goes among `windows`:
    /// centred in the work area, clamped to its corner when it is larger,
    /// and moved right and down while it would lie on another window's
    /// corner and stay inside the work area (MIKSPEC-0074.R4).
    fn place(&self, windows: &[Window], width: i32, height: i32) -> Rect {
        let area = self.work_area();
        let mut x = (area.x + (area.width - width) / 2).max(area.x);
        let mut y = (area.y + (area.height - height) / 2).max(area.y);
        let near = |x: i32, y: i32| {
            windows
                .iter()
                .any(|w| (w.frame.x - x).abs() < NEAR && (w.frame.y - y).abs() < NEAR)
        };
        while near(x, y)
            && x + CASCADE + width <= area.x + area.width
            && y + CASCADE + height <= area.y + area.height
        {
            x += CASCADE;
            y += CASCADE;
        }
        Rect {
            x,
            y,
            width,
            height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::panel::PanelState;
    use super::super::surface::SurfaceState;
    use super::super::{BufferId, Surface};
    use super::*;

    fn toplevel(server: &mut Server, id: SurfaceId, buffer: BufferId, size: (u32, u32)) {
        server.buffer_sizes.insert(buffer, size);
        server.surfaces.insert(
            id,
            Surface {
                client: 1,
                object: 3,
                role: Role::Toplevel,
                pending: SurfaceState::default(),
                attached: false,
                sync: None,
                current: SurfaceState {
                    buffer: Some(buffer),
                    ..SurfaceState::default()
                },
                waiting: Vec::new(),
                pending_frames: Vec::new(),
                frames: Vec::new(),
                xdg: None,
                panel: None,
                material: None,
            },
        );
    }

    fn panel(server: &mut Server, id: SurfaceId, edge: Edge, thickness: u32, exclusive: bool) {
        let mut state = PanelState::new(7, edge);
        state.thickness = thickness;
        state.exclusive = exclusive;
        state.configured = true;
        toplevel(server, id, 90 + id, (0, 0));
        let surface = server.surfaces.get_mut(&id).expect("just made");
        surface.role = Role::Panel;
        surface.panel = Some(state);
    }

    fn frames(server: &Server) -> Vec<(i32, i32)> {
        server
            .windows()
            .iter()
            .map(|w| (w.frame.x, w.frame.y))
            .collect()
    }

    #[test]
    fn the_work_area_leaves_out_exclusive_panels_only() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        assert_eq!(
            server.work_area(),
            Rect {
                x: 0,
                y: 0,
                width: 1280,
                height: 800
            }
        );
        panel(&mut server, 1, Edge::Top, 32, true);
        panel(&mut server, 2, Edge::Bottom, 48, true);
        panel(&mut server, 3, Edge::Left, 100, false);
        assert_eq!(
            server.work_area(),
            Rect {
                x: 0,
                y: 32,
                width: 1280,
                height: 720
            }
        );
    }

    #[test]
    fn a_new_window_is_centred_in_the_work_area() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        panel(&mut server, 1, Edge::Bottom, 48, true);
        toplevel(&mut server, 2, 10, (400, 300));
        assert_eq!(frames(&server), [(440, 226)]);
    }

    #[test]
    fn a_window_larger_than_the_work_area_lies_at_its_corner() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (2000, 900));
        assert_eq!(frames(&server), [(0, 0)]);
    }

    #[test]
    fn windows_that_would_coincide_cascade_while_they_fit() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (400, 300));
        toplevel(&mut server, 2, 11, (400, 300));
        toplevel(&mut server, 3, 12, (400, 300));
        assert_eq!(frames(&server), [(440, 250), (472, 282), (504, 314)]);
        let mut small = Server::new();
        small.set_screen((410, 310));
        toplevel(&mut small, 1, 10, (400, 300));
        toplevel(&mut small, 2, 11, (400, 300));
        assert_eq!(frames(&small), [(5, 5), (5, 5)]);
    }

    #[test]
    fn a_window_keeps_its_place_when_its_buffer_changes_size() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (400, 300));
        assert_eq!(frames(&server), [(440, 250)]);
        server.buffer_sizes.insert(11, (500, 320));
        server.surfaces.get_mut(&1).expect("made").current.buffer = Some(11);
        let windows = server.windows();
        assert_eq!(
            windows[0].frame,
            Rect {
                x: 440,
                y: 250,
                width: 500,
                height: 320
            }
        );
    }

    #[test]
    fn a_window_without_a_buffer_is_unmapped_and_mapped_again_on_top() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (400, 300));
        toplevel(&mut server, 2, 11, (200, 100));
        assert_eq!(server.windows().len(), 2);
        server.surfaces.get_mut(&1).expect("made").current.buffer = None;
        assert_eq!(
            server
                .windows()
                .iter()
                .map(|w| w.surface)
                .collect::<Vec<_>>(),
            [2]
        );
        server.surfaces.get_mut(&1).expect("made").current.buffer = Some(10);
        assert_eq!(
            server
                .windows()
                .iter()
                .map(|w| w.surface)
                .collect::<Vec<_>>(),
            [2, 1]
        );
    }
}
