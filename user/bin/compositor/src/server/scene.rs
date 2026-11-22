//! What a frame shows, bottom first: the windows in their stacking order
//! at their frames, and the configured panels above them, at their
//! edges, each with its buffer once it has one, and the areas of its
//! materials.

use super::{BufferId, Rect, Role, Server, SurfaceId};

/// A surface that a frame shows.
#[derive(Debug, PartialEq, Eq)]
pub struct Shown {
    pub surface: SurfaceId,
    /// The window or panel whose tree it belongs to, which is `surface`
    /// for the window or panel itself.
    pub root: SurfaceId,
    pub buffer: BufferId,
    pub x: i32,
    pub y: i32,
    /// The size that it is drawn at, which is its own unless a window is
    /// stretched to its frame (MIKSPEC-0074.R15).
    pub width: i32,
    pub height: i32,
    /// The surface's own size, which its viewport may set, and the part of
    /// its buffer that it shows, if its viewport crops it (24.8 fixed-point).
    pub extent: (i32, i32),
    pub source: Option<[i32; 4]>,
    pub haze: Vec<Rect>,
    pub blur: Vec<Rect>,
}

/// How to make the entry of a surface that lies at a place.
type MakeShown<'a> = &'a dyn Fn(
    SurfaceId,
    SurfaceId,
    BufferId,
    i32,
    i32,
    Option<(i32, i32)>,
    &super::Surface,
) -> Shown;

impl Server {
    /// Adds the entries of the tree of the window or panel `own` to
    /// `out`, bottom first: each surface between its subsurfaces below it
    /// and those above it. A subsurface without a buffer hides its own
    /// subsurfaces too.
    fn push_tree(&self, own: Shown, make: MakeShown<'_>, out: &mut Vec<Shown>) {
        enum Work {
            Visit(SurfaceId, (i32, i32)),
            Emit(Shown),
        }
        let root = own.surface;
        let mut own = Some(own);
        let mut work = vec![Work::Visit(
            root,
            (
                own.as_ref().map_or(0, |o| o.x),
                own.as_ref().map_or(0, |o| o.y),
            ),
        )];
        while let Some(item) = work.pop() {
            match item {
                Work::Emit(entry) => out.push(entry),
                Work::Visit(id, at) => {
                    let mut paint = Vec::new();
                    for member in self.subsurfaces.stack(id) {
                        if member == id {
                            let entry = if id == root {
                                own.take()
                            } else {
                                self.surfaces.get(&id).and_then(|s| {
                                    Some(make(id, root, s.current.buffer?, at.0, at.1, None, s))
                                })
                            };
                            paint.extend(entry.map(Work::Emit));
                        } else if self
                            .surfaces
                            .get(&member)
                            .is_some_and(|s| s.current.buffer.is_some())
                        {
                            let (dx, dy) = self.subsurfaces.position(member);
                            paint.push(Work::Visit(member, (at.0 + dx, at.1 + dy)));
                        }
                    }
                    work.extend(paint.into_iter().rev());
                }
            }
        }
    }

    /// The windows, bottom first, then the configured panels.
    pub fn shown(&self) -> Vec<Shown> {
        let screen = self.screen.unwrap_or((0, 0));
        let shown = |id, root, buffer, x, y, dest: Option<(i32, i32)>, s: &super::Surface| {
            let extent = self.extent(s);
            let (width, height) = dest.unwrap_or(extent);
            Shown {
                surface: id,
                root,
                buffer,
                x,
                y,
                width,
                height,
                extent,
                source: s.current.viewport.source,
                haze: s.current.materials.haze.rects().to_vec(),
                blur: s.current.materials.blur.rects().to_vec(),
            }
        };
        let mut shown_windows = Vec::new();
        for window in self.windows().into_iter().filter(|w| !w.minimized) {
            let Some(s) = self.surfaces.get(&window.surface) else {
                continue;
            };
            let Some(buffer) = s.current.buffer else {
                continue;
            };
            let dest = Some((window.frame.width, window.frame.height));
            let (id, x, y) = (window.surface, window.frame.x, window.frame.y);
            let own = shown(id, id, buffer, x, y, dest, s);
            self.push_tree(own, &shown, &mut shown_windows);
        }
        for (&id, s) in &self.surfaces {
            let Some(buffer) = s.current.buffer else {
                continue;
            };
            match (s.role, &s.panel) {
                (Role::Panel, Some(panel)) if panel.configured => {
                    let place = panel.place(screen);
                    let own = shown(id, id, buffer, place.x, place.y, None, s);
                    self.push_tree(own, &shown, &mut shown_windows);
                }
                _ => {}
            }
        }
        shown_windows
    }
}

#[cfg(test)]
mod tests {
    use super::super::Surface;
    use super::super::panel::{Edge, PanelState};
    use super::super::surface::SurfaceState;
    use super::*;

    fn surface(role: Role, buffer: Option<BufferId>, panel: Option<PanelState>) -> Surface {
        Surface {
            client: 1,
            object: 3,
            role,
            pending: SurfaceState::default(),
            attached: false,
            sync: None,
            current: SurfaceState {
                buffer,
                ..SurfaceState::default()
            },
            waiting: Vec::new(),
            pending_frames: Vec::new(),
            frames: Vec::new(),
            xdg: None,
            panel,
            material: None,
        }
    }

    #[test]
    fn shows_toplevels_below_configured_panels() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        let mut configured = PanelState::new(7, Edge::Bottom);
        configured.thickness = 48;
        configured.configured = true;
        let waiting = PanelState::new(8, Edge::Top);
        server
            .surfaces
            .insert(1, surface(Role::Panel, Some(10), Some(configured)));
        server
            .surfaces
            .insert(2, surface(Role::Toplevel, Some(11), None));
        server
            .surfaces
            .insert(3, surface(Role::Panel, Some(12), Some(waiting)));
        server
            .surfaces
            .insert(4, surface(Role::Toplevel, None, None));
        server
            .surfaces
            .insert(5, surface(Role::None, Some(13), None));
        server.buffer_sizes.insert(11, (400, 300));
        let shown: Vec<_> = server
            .shown()
            .iter()
            .map(|s| (s.buffer, s.x, s.y))
            .collect();
        assert_eq!(shown, [(11, 440, 250), (10, 0, 752)]);
    }
}
