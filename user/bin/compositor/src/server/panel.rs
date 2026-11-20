//! `mikros_panel_v1` (MIKSPEC-0076.R3): a surface along the whole length of
//! an edge of the output, as thick as its client asks. The compositor
//! sends `configure` with the panel's size after each commit that follows
//! a change of its thickness and whenever the screen's size changes, and
//! shows the panel's buffer, at the edge, only once the client
//! acknowledged a configure.

use super::{ProtocolError, Rect, Request, Role, Server, SurfaceId};
use crate::wayland::protocols::mikros_panel_v1;
use crate::wayland::wire::{Arg, ObjectId};

/// The protocol error of a panel request that breaks the protocol, for
/// which the protocol file defines no code.
pub const PANEL_ERROR: u32 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    pub fn of(value: u32) -> Option<Edge> {
        Some(match value {
            1 => Edge::Top,
            2 => Edge::Bottom,
            3 => Edge::Left,
            4 => Edge::Right,
            _ => return None,
        })
    }
}

#[derive(Debug)]
pub struct PanelState {
    pub object: ObjectId,
    pub edge: Edge,
    pub thickness: u32,
    pub exclusive: bool,
    /// The size of the last configure, once one was sent, and the serials
    /// that the client has not yet acknowledged.
    sent_size: Option<(u32, u32)>,
    sent: Vec<u32>,
    pub configured: bool,
}

impl PanelState {
    pub fn new(object: ObjectId, edge: Edge) -> PanelState {
        PanelState {
            object,
            edge,
            thickness: 0,
            exclusive: false,
            sent_size: None,
            sent: Vec::new(),
            configured: false,
        }
    }

    /// The panel's size on a screen of `screen`.
    fn size(&self, (width, height): (i32, i32)) -> (u32, u32) {
        let (width, height) = (width.max(0) as u32, height.max(0) as u32);
        match self.edge {
            Edge::Top | Edge::Bottom => (width, self.thickness),
            Edge::Left | Edge::Right => (self.thickness, height),
        }
    }

    /// Where the panel lies on a screen of `screen`.
    pub fn place(&self, screen: (i32, i32)) -> Rect {
        let (width, height) = self.size(screen);
        let (width, height) = (width as i32, height as i32);
        let (x, y) = match self.edge {
            Edge::Top | Edge::Left => (0, 0),
            Edge::Bottom => (0, screen.1 - height),
            Edge::Right => (screen.0 - width, 0),
        };
        Rect {
            x,
            y,
            width,
            height,
        }
    }
}

impl Server {
    pub(super) fn panel_request(
        &mut self,
        request: Request,
        id: SurfaceId,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let decoded = mikros_panel_v1::Request::decode(request.opcode, args)
            .map_err(|e| ProtocolError::invalid_method(request.object, format!("{e:?}")))?;
        let panel = self
            .surfaces
            .get_mut(&id)
            .and_then(|s| s.panel.as_mut())
            .filter(|p| p.object == request.object);
        match decoded {
            mikros_panel_v1::Request::Destroy => {
                if let Some(surface) = self.surfaces.get_mut(&id) {
                    surface.panel = None;
                }
                self.client_mut(request).destroy(request.object);
            }
            // The requests of a panel whose surface is gone do nothing.
            _ if panel.is_none() => {}
            mikros_panel_v1::Request::SetThickness { thickness } => {
                panel.expect("a panel").thickness = thickness;
            }
            mikros_panel_v1::Request::SetExclusive { exclusive } => {
                panel.expect("a panel").exclusive = exclusive == 1;
            }
            mikros_panel_v1::Request::AckConfigure { serial } => {
                let panel = panel.expect("a panel");
                let Some(at) = panel.sent.iter().position(|&s| s == serial) else {
                    return Err(ProtocolError::new(
                        request.object,
                        PANEL_ERROR,
                        "an acknowledged serial that was not sent",
                    ));
                };
                panel.sent.drain(..=at);
                panel.configured = true;
            }
        }
        Ok(())
    }

    /// Gives the surface `surface` of the request's client the role of a
    /// panel `object` along `edge`.
    pub(super) fn make_panel(
        &mut self,
        request: Request,
        object: ObjectId,
        surface: SurfaceId,
        edge: u32,
    ) -> Result<(), ProtocolError> {
        let error = |message: &str| ProtocolError::new(request.object, PANEL_ERROR, message);
        let edge = Edge::of(edge).ok_or_else(|| error("an edge that is not 1 to 4"))?;
        let s = self.surface_mut(surface)?;
        if s.role != Role::None || s.xdg.is_some() {
            return Err(error("a panel of a surface that has a role"));
        }
        s.role = Role::Panel;
        s.panel = Some(PanelState::new(object, edge));
        Ok(())
    }

    /// Configures the panel of `id` after a commit when its size is not
    /// the one last sent.
    pub(super) fn panel_committed(&mut self, id: SurfaceId) {
        self.configure_panel(id, false);
    }

    /// Sends `configure` to the panel of `id` with its size on the
    /// screen, unless that size was sent last and `always` is false.
    fn configure_panel(&mut self, id: SurfaceId, always: bool) {
        let screen = self.screen.unwrap_or((0, 0));
        let serial = self.serial.wrapping_add(1);
        let Some(surface) = self.surfaces.get_mut(&id) else {
            return;
        };
        let Some(panel) = surface.panel.as_mut() else {
            return;
        };
        let size = panel.size(screen);
        if panel.sent_size == Some(size) && !always {
            return;
        }
        let Some(c) = self.clients.get_mut(&surface.client) else {
            return;
        };
        self.serial = serial;
        panel.sent_size = Some(size);
        panel.sent.push(serial);
        mikros_panel_v1::send_configure(&mut c.out, panel.object, serial, size.0, size.1);
    }

    /// Makes `screen` the screen's size, and configures every panel when
    /// it changed.
    pub fn set_screen(&mut self, screen: (i32, i32)) {
        if self.screen == Some(screen) {
            return;
        }
        self.screen = Some(screen);
        let panels: Vec<SurfaceId> = self
            .surfaces
            .iter()
            .filter(|(_, s)| s.panel.as_ref().is_some_and(|p| p.sent_size.is_some()))
            .map(|(&id, _)| id)
            .collect();
        for id in panels {
            self.configure_panel(id, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestClient;
    use super::*;
    use crate::wayland::protocols::{mikros_shell_v1, wl_compositor, wl_output, wl_registry};
    use crate::wayland::wire::NewId;

    /// A taskbar whose registry is 2, compositor 3, output 4, shell 5,
    /// surface 6 and panel 7 at `edge`.
    fn panel(server: &mut Server, roles: &[&str], edge: u32) -> TestClient {
        let roles = roles.iter().map(|r| r.to_string()).collect();
        let client = TestClient {
            id: server.connect_component(roles),
            imports: Default::default(),
        };
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(server, 2, &wl_compositor::INTERFACE, 3);
        client.bind(server, 2, &wl_output::INTERFACE, 4);
        client.bind(server, 2, &mikros_shell_v1::INTERFACE, 5);
        client.send(server, 3, 0, vec![Arg::NewId(NewId(6))]);
        let args = vec![
            Arg::NewId(NewId(7)),
            Arg::Object(Some(6)),
            Arg::Object(Some(4)),
            Arg::Uint(edge),
        ];
        client.send(server, 5, 0, args);
        client
    }

    /// The configure events of the panel 7: serial, width and height.
    fn configures(server: &mut Server, client: &TestClient) -> Vec<Vec<Arg>> {
        let events = client.events(server, &|id| match id {
            4 => &wl_output::EVENTS,
            7 => &mikros_panel_v1::EVENTS,
            _ => &wl_registry::EVENTS,
        });
        events
            .into_iter()
            .filter(|e| (e.0, e.1) == (7, 0))
            .map(|e| e.2)
            .collect()
    }

    fn the_panel(server: &Server) -> &PanelState {
        let (_, surface) = server.surfaces().next().unwrap();
        surface.panel.as_ref().unwrap()
    }

    #[test]
    fn configures_a_panel_with_its_size_after_a_commit() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        let client = panel(&mut server, &["taskbar"], 2);
        client.send(&mut server, 7, 0, vec![Arg::Uint(48)]);
        client.send(&mut server, 6, 6, vec![]);
        assert_eq!(client.error(&mut server), None);
        let sent = configures(&mut server, &client);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0][1..], [Arg::Uint(1280), Arg::Uint(48)]);
        // Another commit of the same size sends nothing.
        client.send(&mut server, 6, 6, vec![]);
        assert!(configures(&mut server, &client).is_empty());
        let Arg::Uint(serial) = sent[0][0] else {
            panic!("a serial");
        };
        client.send(&mut server, 7, 2, vec![Arg::Uint(serial)]);
        let panel = the_panel(&server);
        assert!(panel.configured);
        let place = Rect {
            x: 0,
            y: 752,
            width: 1280,
            height: 48,
        };
        assert_eq!(panel.place((1280, 800)), place);
        server.set_screen((1024, 768));
        let sent = configures(&mut server, &client);
        assert_eq!(sent[0][1..], [Arg::Uint(1024), Arg::Uint(48)]);
    }

    #[test]
    fn places_panels_along_each_edge() {
        let mut state = PanelState::new(7, Edge::Right);
        state.thickness = 40;
        let rect = |x, y, width, height| Rect {
            x,
            y,
            width,
            height,
        };
        assert_eq!(state.place((1280, 800)), rect(1240, 0, 40, 800));
        state.edge = Edge::Top;
        assert_eq!(state.place((1280, 800)), rect(0, 0, 1280, 40));
        state.edge = Edge::Left;
        assert_eq!(state.place((1280, 800)), rect(0, 0, 40, 800));
    }

    #[test]
    fn refuses_panels_of_other_roles_and_edges() {
        let mut server = Server::new();
        let client = panel(&mut server, &["desktop"], 2);
        assert_eq!(
            client.error(&mut server),
            Some(super::super::shell::SHELL_ERROR)
        );
        let mut server = Server::new();
        let client = panel(&mut server, &["menubar"], 5);
        assert_eq!(client.error(&mut server), Some(PANEL_ERROR));
    }
}
