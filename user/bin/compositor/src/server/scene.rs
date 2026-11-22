//! What a frame shows, bottom first: the windows in their stacking order
//! at their frames, and the configured panels above them, at their
//! edges, each with its buffer once it has one, and the areas of its
//! materials.

use super::{BufferId, Rect, Role, Server, SurfaceId};

/// A surface that a frame shows.
#[derive(Debug, PartialEq, Eq)]
pub struct Shown {
    pub surface: SurfaceId,
    pub buffer: BufferId,
    pub x: i32,
    pub y: i32,
    pub haze: Vec<Rect>,
    pub blur: Vec<Rect>,
}

impl Server {
    /// The windows, bottom first, then the configured panels.
    pub fn shown(&self) -> Vec<Shown> {
        let screen = self.screen.unwrap_or((0, 0));
        let shown = |id, buffer, x, y, s: &super::Surface| Shown {
            surface: id,
            buffer,
            x,
            y,
            haze: s.current.materials.haze.rects().to_vec(),
            blur: s.current.materials.blur.rects().to_vec(),
        };
        let mut shown_windows = Vec::new();
        for window in self.windows().into_iter().filter(|w| !w.minimized) {
            let Some(s) = self.surfaces.get(&window.surface) else {
                continue;
            };
            let Some(buffer) = s.current.buffer else {
                continue;
            };
            shown_windows.push(shown(
                window.surface,
                buffer,
                window.frame.x,
                window.frame.y,
                s,
            ));
        }
        for (&id, s) in &self.surfaces {
            let Some(buffer) = s.current.buffer else {
                continue;
            };
            match (s.role, &s.panel) {
                (Role::Panel, Some(panel)) if panel.configured => {
                    let place = panel.place(screen);
                    shown_windows.push(shown(id, buffer, place.x, place.y, s));
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
