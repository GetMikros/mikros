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
    /// Whether the window is minimized: not drawn, and not a place for
    /// focus, but still a window.
    pub minimized: bool,
    /// Where the window lay before it was maximized or made fullscreen,
    /// which it goes back to.
    restore: Option<Rect>,
    /// Whether the window demands attention: it was mapped below the
    /// focused window, and has not had the keyboard focus since.
    pub attention: bool,
    /// The size that a resize gave the frame, which the frame keeps while
    /// the window's buffer is of another size, until a buffer of that size
    /// comes (MIKSPEC-0074.R15). The buffer's size gives it otherwise.
    pub target: Option<(i32, i32)>,
}

/// The windows, bottom first.
#[derive(Debug, Default)]
pub(super) struct Windows {
    stack: RefCell<Vec<Window>>,
    /// The windows that were mapped and are to get the keyboard focus.
    to_focus: RefCell<Vec<SurfaceId>>,
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
                    area.y = area.y.saturating_add(zone.height.max(0));
                    area.height = area.height.saturating_sub(zone.height.max(0));
                }
                Edge::Bottom => area.height = area.height.saturating_sub(zone.height.max(0)),
                Edge::Left => {
                    area.x = area.x.saturating_add(zone.width.max(0));
                    area.width = area.width.saturating_sub(zone.width.max(0));
                }
                Edge::Right => area.width = area.width.saturating_sub(zone.width.max(0)),
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
        let area = self.work_area();
        let mapped: Vec<(SurfaceId, (i32, i32), bool)> = self
            .surfaces
            .iter()
            .filter(|(_, s)| s.role == Role::Toplevel)
            .filter_map(|(&id, s)| {
                let (w, h) = size(s.current.buffer?);
                let minimized = s.xdg.as_ref().is_some_and(|x| x.minimized);
                let to_i32 = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
                Some((id, (to_i32(w), to_i32(h)), minimized))
            })
            .collect();
        stack.retain_mut(|w| match mapped.iter().find(|(id, ..)| *id == w.surface) {
            Some(&(_, (width, height), minimized)) => {
                let before = w.frame;
                // A frame that a resize sized keeps its size until a buffer
                // of that size is there and no resize goes on.
                let resizing = self.pointer.resizing_surface() == Some(w.surface);
                if w.target == Some((width, height)) && !resizing {
                    w.target = None;
                }
                let (width, height) = w.target.unwrap_or((width, height));
                w.frame.width = width;
                w.frame.height = height;
                w.minimized = minimized;
                // A fullscreen window lies at its output's corner and a
                // maximized one at its work area's.
                let state = self.surfaces.get(&w.surface).and_then(|s| s.xdg.as_ref());
                let corner = if state.is_some_and(|x| x.fullscreen) {
                    Some((0, 0))
                } else if state.is_some_and(|x| x.maximized) {
                    Some((area.x, area.y))
                } else {
                    None
                };
                match corner {
                    Some(corner) => {
                        w.restore.get_or_insert(before);
                        (w.frame.x, w.frame.y) = corner;
                        // The size is the buffer's, which the client draws
                        // at the size it was asked for.
                        w.target = None;
                        w.frame.width = width;
                        w.frame.height = height;
                    }
                    None => {
                        if let Some(r) = w.restore.take() {
                            (w.frame.x, w.frame.y) = (r.x, r.y);
                        }
                    }
                }
                true
            }
            None => false,
        });
        for &(id, (width, height), minimized) in &mapped {
            if stack.iter().any(|w| w.surface == id) {
                continue;
            }
            let parent_id = self.parent_of(id);
            let parent = parent_id
                .and_then(|p| stack.iter().find(|w| w.surface == p))
                .map(|w| w.frame);
            let frame = self.place(&stack, width, height, parent);
            // A new window gets the focus and the top when it was
            // activated with a token, when it is a child of the focused
            // window or when no window has the focus; otherwise it goes
            // below the focused window, but above its parent, and demands
            // attention (MIKSPEC-0074.R28).
            let focused = self
                .keyboard_focus()
                .filter(|f| stack.iter().any(|w| w.surface == *f && !w.minimized));
            let focus = self.activation.waiting.contains(&id)
                || focused.is_none()
                || (parent_id.is_some() && parent_id == focused);
            let window = Window {
                surface: id,
                frame,
                minimized,
                restore: None,
                attention: !focus,
                target: None,
            };
            if focus {
                stack.push(window);
                self.windows.to_focus.borrow_mut().push(id);
            } else {
                let focused_at = stack.iter().position(|w| Some(w.surface) == focused);
                let parent_at = parent_id.and_then(|p| stack.iter().position(|w| w.surface == p));
                let at = focused_at
                    .unwrap_or(stack.len())
                    .max(parent_at.map_or(0, |p| p + 1));
                stack.insert(at, window);
            }
        }
        stack.clone()
    }

    /// Gives the keyboard focus to the newest of the windows that were
    /// mapped with a right to it since the last call.
    pub(super) fn focus_new_windows(&mut self) {
        self.windows();
        let fresh = std::mem::take(&mut *self.windows.to_focus.borrow_mut());
        for id in &fresh {
            self.activation.waiting.remove(id);
        }
        let surfaces = &self.surfaces;
        self.activation.waiting.retain(|s| surfaces.contains_key(s));
        if let Some(&id) = fresh.last() {
            self.focus_keyboard(Some(id));
        }
    }

    /// Clears the attention that the window of `surface` demands, as it has
    /// the keyboard focus now.
    pub(super) fn clear_attention(&self, surface: SurfaceId) {
        if let Some(w) = self
            .windows
            .stack
            .borrow_mut()
            .iter_mut()
            .find(|w| w.surface == surface)
        {
            w.attention = false;
        }
    }

    fn parent_of(&self, surface: SurfaceId) -> Option<SurfaceId> {
        self.surfaces.get(&surface)?.xdg.as_ref()?.parent
    }

    /// Raises the window of `surface`, and its children with it, keeping
    /// their order, to the top of the stack (MIKSPEC-0074.R5).
    pub fn raise(&mut self, surface: SurfaceId) {
        self.windows();
        let descends = |mut id: SurfaceId| {
            while let Some(parent) = self.parent_of(id) {
                if parent == surface {
                    return true;
                }
                id = parent;
            }
            false
        };
        let before: Vec<SurfaceId> = self.windows().iter().map(|w| w.surface).collect();
        let (raised, kept): (Vec<Window>, Vec<Window>) = self
            .windows
            .stack
            .borrow_mut()
            .drain(..)
            .partition(|w| w.surface == surface || descends(w.surface));
        let frames: Vec<Rect> = raised.iter().map(|w| w.frame).collect();
        let mut stack = self.windows.stack.borrow_mut();
        *stack = kept;
        stack.extend(raised);
        // Windows that now overlap in another order are drawn again.
        if stack.iter().map(|w| w.surface).ne(before) {
            self.restacked.extend(frames);
        }
    }

    /// The frame that the window of `surface` had before it was maximized
    /// or made fullscreen, which it no longer goes back to.
    pub(super) fn take_restore(&self, surface: SurfaceId) -> Option<Rect> {
        self.windows();
        self.windows
            .stack
            .borrow_mut()
            .iter_mut()
            .find(|w| w.surface == surface)?
            .restore
            .take()
    }

    /// Gives the window of `surface` the frame `frame`, whose size it keeps
    /// while its buffer is of another size.
    pub(super) fn set_frame(&self, surface: SurfaceId, frame: Rect) {
        self.windows();
        if let Some(w) = self
            .windows
            .stack
            .borrow_mut()
            .iter_mut()
            .find(|w| w.surface == surface)
        {
            w.frame = frame;
            w.target = Some((frame.width, frame.height));
        }
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

    /// Where a new window of `width` by `height` goes among `windows`: over
    /// its `parent`, clamped to the work area, or else centred in the work area, clamped to its corner when it is larger,
    /// and moved right and down while it would lie on another window's
    /// corner and stay inside the work area (MIKSPEC-0074.R4).
    fn place(&self, windows: &[Window], width: i32, height: i32, parent: Option<Rect>) -> Rect {
        let area = self.work_area();
        // In 64 bits, so that sizes of clients cannot overflow.
        let (ax, ay, aw, ah) = (
            i64::from(area.x),
            i64::from(area.y),
            i64::from(area.width),
            i64::from(area.height),
        );
        let (w, h) = (i64::from(width), i64::from(height));
        let to_i32 = |v: i64| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        if let Some(parent) = parent {
            let x = (i64::from(parent.x) + (i64::from(parent.width) - w) / 2).min(ax + aw - w);
            let y = (i64::from(parent.y) + (i64::from(parent.height) - h) / 2).min(ay + ah - h);
            return Rect {
                x: to_i32(x.max(ax)),
                y: to_i32(y.max(ay)),
                width,
                height,
            };
        }
        let mut x = (ax + (aw - w) / 2).max(ax);
        let mut y = (ay + (ah - h) / 2).max(ay);
        let near = |x: i64, y: i64| {
            windows.iter().any(|f| {
                (i64::from(f.frame.x) - x).abs() < i64::from(NEAR)
                    && (i64::from(f.frame.y) - y).abs() < i64::from(NEAR)
            })
        };
        let step = i64::from(CASCADE);
        while near(x, y) && x + step + w <= ax + aw && y + step + h <= ay + ah {
            x += step;
            y += step;
        }
        Rect {
            x: to_i32(x),
            y: to_i32(y),
            width,
            height,
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::panel::PanelState;
    use super::super::surface::SurfaceState;
    use super::super::xdg::XdgState;
    use super::super::{BufferId, Surface};
    use super::*;

    pub(in super::super) fn toplevel(
        server: &mut Server,
        id: SurfaceId,
        buffer: BufferId,
        size: (u32, u32),
    ) {
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

    pub(in super::super) fn panel(
        server: &mut Server,
        id: SurfaceId,
        edge: Edge,
        thickness: u32,
        exclusive: bool,
    ) {
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

    fn child(server: &mut Server, id: SurfaceId, parent: SurfaceId) {
        server.surfaces.get_mut(&id).expect("made").xdg =
            Some(XdgState::windowed(Some(parent), false));
    }

    fn order(server: &Server) -> Vec<SurfaceId> {
        server.windows().iter().map(|w| w.surface).collect()
    }

    #[test]
    fn raising_a_window_raises_its_children_and_keeps_them_above_it() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (400, 300));
        toplevel(&mut server, 2, 11, (200, 100));
        toplevel(&mut server, 3, 12, (300, 200));
        child(&mut server, 2, 1);
        assert_eq!(order(&server), [1, 2, 3]);
        server.raise(1);
        assert_eq!(order(&server), [3, 1, 2]);
        server.raise(3);
        assert_eq!(order(&server), [1, 2, 3]);
        server.raise(2);
        assert_eq!(order(&server), [1, 3, 2]);
    }

    #[test]
    fn a_child_is_centred_over_its_parent_within_the_work_area() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (400, 300));
        server.move_window(1, 0, 0);
        toplevel(&mut server, 2, 11, (200, 100));
        child(&mut server, 2, 1);
        toplevel(&mut server, 3, 12, (300, 400));
        child(&mut server, 3, 1);
        let frames: Vec<_> = server.windows().iter().map(|w| w.frame).collect();
        assert_eq!((frames[1].x, frames[1].y), (100, 100));
        assert_eq!((frames[2].x, frames[2].y), (50, 0));
    }

    #[test]
    fn minimized_windows_stay_windows_but_are_not_shown() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (400, 300));
        toplevel(&mut server, 2, 11, (200, 100));
        server.surfaces.get_mut(&2).expect("made").xdg = Some(XdgState::windowed(None, true));
        let windows = server.windows();
        assert_eq!(
            windows.iter().map(|w| w.minimized).collect::<Vec<_>>(),
            [false, true]
        );
        assert_eq!(
            server.shown().iter().map(|s| s.surface).collect::<Vec<_>>(),
            [1]
        );
    }

    #[test]
    fn raising_a_window_damages_its_frame_only_when_the_order_changes() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (400, 300));
        toplevel(&mut server, 2, 11, (400, 300));
        let first = server.windows()[0].frame;
        server.take_damage();
        assert!(server.take_damage().is_empty());
        server.raise(2);
        assert!(server.take_damage().is_empty());
        server.raise(1);
        assert_eq!(server.take_damage(), [first]);
        assert!(server.take_damage().is_empty());
    }

    #[test]
    fn a_window_that_is_maximized_or_fullscreen_after_it_is_mapped_moves_to_its_corner() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        panel(&mut server, 1, Edge::Top, 32, true);
        toplevel(&mut server, 2, 10, (400, 300));
        assert_eq!(frames(&server), [(440, 266)]);
        let mut state = XdgState::windowed(None, false);
        state.maximized = true;
        server.surfaces.get_mut(&2).expect("made").xdg = Some(state);
        server.buffer_sizes.insert(11, (1280, 768));
        server.surfaces.get_mut(&2).expect("made").current.buffer = Some(11);
        assert_eq!(frames(&server), [(0, 32)]);
        let mut state = XdgState::windowed(None, false);
        state.fullscreen = true;
        server.surfaces.get_mut(&2).expect("made").xdg = Some(state);
        server.buffer_sizes.insert(12, (1280, 800));
        server.surfaces.get_mut(&2).expect("made").current.buffer = Some(12);
        assert_eq!(frames(&server), [(0, 0)]);
    }

    #[test]
    fn a_window_goes_back_where_it_was_when_it_is_no_longer_maximized() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (400, 300));
        assert_eq!(frames(&server), [(440, 250)]);
        let mut state = XdgState::windowed(None, false);
        state.maximized = true;
        server.surfaces.get_mut(&1).expect("made").xdg = Some(state);
        assert_eq!(frames(&server), [(0, 0)]);
        server.surfaces.get_mut(&1).expect("made").xdg = Some(XdgState::windowed(None, false));
        assert_eq!(frames(&server), [(440, 250)]);
    }

    #[test]
    fn huge_panels_and_buffers_do_not_overflow() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        panel(&mut server, 1, Edge::Top, u32::MAX, true);
        toplevel(&mut server, 2, 10, (u32::MAX, u32::MAX));
        let area = server.work_area();
        assert!(area.width >= 0 && area.height >= 0);
        assert_eq!(server.windows().len(), 1);
    }

    fn attention(server: &Server) -> Vec<(SurfaceId, bool)> {
        server
            .windows()
            .iter()
            .map(|w| (w.surface, w.attention))
            .collect()
    }

    #[test]
    fn the_first_window_gets_the_focus_and_the_next_ones_wait_below_it() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (400, 300));
        server.focus_new_windows();
        assert_eq!(server.keyboard_focus(), Some(1));
        toplevel(&mut server, 2, 11, (400, 300));
        toplevel(&mut server, 3, 12, (400, 300));
        server.focus_new_windows();
        assert_eq!(server.keyboard_focus(), Some(1));
        assert_eq!(order(&server), [2, 3, 1]);
        assert_eq!(attention(&server), [(2, true), (3, true), (1, false)]);
    }

    #[test]
    fn a_window_with_a_token_or_a_child_of_the_focused_window_gets_the_focus() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (400, 300));
        server.focus_new_windows();
        server.activation.waiting.insert(2);
        toplevel(&mut server, 2, 11, (400, 300));
        server.focus_new_windows();
        assert_eq!(server.keyboard_focus(), Some(2));
        assert!(server.activation.waiting.is_empty());
        toplevel(&mut server, 3, 12, (200, 100));
        child(&mut server, 3, 2);
        server.focus_new_windows();
        assert_eq!(server.keyboard_focus(), Some(3));
        assert_eq!(order(&server), [1, 2, 3]);
        assert_eq!(attention(&server), [(1, false), (2, false), (3, false)]);
    }

    #[test]
    fn a_child_below_the_focused_window_stays_above_its_parent() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (400, 300));
        toplevel(&mut server, 2, 11, (400, 300));
        server.focus_new_windows();
        server.focus_keyboard(Some(2));
        assert_eq!(order(&server), [1, 2]);
        // A child of window 1 while window 2 has the focus.
        toplevel(&mut server, 3, 12, (200, 100));
        child(&mut server, 3, 1);
        server.focus_new_windows();
        assert_eq!(server.keyboard_focus(), Some(2));
        assert_eq!(order(&server), [1, 3, 2]);
        assert_eq!(attention(&server), [(1, false), (3, true), (2, false)]);
    }

    #[test]
    fn attention_ends_when_the_window_gets_the_focus() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (400, 300));
        server.focus_new_windows();
        toplevel(&mut server, 2, 11, (400, 300));
        server.focus_new_windows();
        assert_eq!(attention(&server), [(2, true), (1, false)]);
        server.focus_keyboard(Some(2));
        assert_eq!(attention(&server), [(2, false), (1, false)]);
    }

    #[test]
    fn a_frame_keeps_the_size_a_resize_gave_it_until_a_buffer_of_that_size_comes() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        toplevel(&mut server, 1, 10, (400, 300));
        server.set_frame(
            1,
            Rect {
                x: 10,
                y: 20,
                width: 600,
                height: 450,
            },
        );
        let shown = server.shown();
        assert_eq!(
            (shown[0].x, shown[0].y, shown[0].width, shown[0].height),
            (10, 20, 600, 450)
        );
        // The client draws another size that is not the frame's: still stretched.
        server.buffer_sizes.insert(11, (500, 380));
        server.surfaces.get_mut(&1).expect("made").current.buffer = Some(11);
        assert_eq!(
            (server.shown()[0].width, server.shown()[0].height),
            (600, 450)
        );
        // A buffer of the frame's size: drawn as it is, and the frame follows
        // the buffer from then on.
        server.buffer_sizes.insert(12, (600, 450));
        server.surfaces.get_mut(&1).expect("made").current.buffer = Some(12);
        assert_eq!(server.windows()[0].target, None);
        server.buffer_sizes.insert(13, (640, 480));
        server.surfaces.get_mut(&1).expect("made").current.buffer = Some(13);
        assert_eq!(
            (server.shown()[0].width, server.shown()[0].height),
            (640, 480)
        );
    }
}
