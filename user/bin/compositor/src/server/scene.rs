//! What a frame shows, bottom first: the toplevels, oldest surface first,
//! at the output's corner, and the configured panels above them, at
//! their edges, each with its buffer once it has one, and the areas of
//! its materials.

use super::{BufferId, Point, Rect, Role, Server, SurfaceId};

/// A surface that a frame shows.
#[derive(Debug, PartialEq, Eq)]
pub struct Shown {
    pub surface: SurfaceId,
    pub buffer: BufferId,
    pub acquire: Option<Point>,
    pub x: i32,
    pub y: i32,
    pub haze: Vec<Rect>,
    pub blur: Vec<Rect>,
}

impl Server {
    pub fn shown(&self) -> Vec<Shown> {
        let screen = self.screen.unwrap_or((0, 0));
        let mut toplevels = Vec::new();
        let mut panels = Vec::new();
        for (&id, s) in &self.surfaces {
            let Some(buffer) = s.current.buffer else {
                continue;
            };
            let (x, y) = match (s.role, &s.panel) {
                (Role::Toplevel, _) => (0, 0),
                (Role::Panel, Some(panel)) if panel.configured => {
                    let place = panel.place(screen);
                    (place.x, place.y)
                }
                _ => continue,
            };
            let shown = Shown {
                surface: id,
                buffer,
                acquire: s.current.acquire,
                x,
                y,
                haze: s.current.materials.haze.rects().to_vec(),
                blur: s.current.materials.blur.rects().to_vec(),
            };
            match s.role {
                Role::Panel => panels.push(shown),
                _ => toplevels.push(shown),
            }
        }
        toplevels.extend(panels);
        toplevels
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
            pending_frames: Vec::new(),
            frames: Vec::new(),
            xdg: None,
            panel,
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
        let shown: Vec<_> = server
            .shown()
            .iter()
            .map(|s| (s.buffer, s.x, s.y))
            .collect();
        assert_eq!(shown, [(11, 0, 0), (10, 0, 752)]);
    }
}
