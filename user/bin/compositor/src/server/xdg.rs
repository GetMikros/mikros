//! `xdg_wm_base`, `xdg_positioner`, `xdg_surface` and `xdg_toplevel`:
//! surfaces with the toplevel role, the state that their client asks for,
//! and the configure events that the client acknowledges before it may
//! commit a buffer. Popups are not served yet.

use super::{ProtocolError, Request, Resource, Role, Server, SurfaceId};
use crate::wayland::protocols::{
    wl_surface, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base,
};
use crate::wayland::wire::{Arg, DecodeError, ObjectId};

/// The state of a surface's xdg_surface and of its toplevel.
#[derive(Debug, Default)]
pub struct XdgState {
    /// The xdg_surface object, and the xdg_toplevel object once it has one.
    pub object: ObjectId,
    pub toplevel: Option<ObjectId>,
    /// The serials of the configure events that the client has not yet
    /// acknowledged, oldest first.
    pub(super) sent: Vec<u32>,
    /// Whether the client acknowledged a configure event, and so may
    /// commit a buffer.
    pub configured: bool,
    /// Whether the initial commit was made, after which every change of
    /// the toplevel's state is configured.
    pub(super) initialized: bool,
    pub title: String,
    pub app_id: String,
    pub fullscreen: bool,
    pub maximized: bool,
    pub min_size: (i32, i32),
    pub max_size: (i32, i32),
    pub geometry: Option<super::Rect>,
    /// The surface of the toplevel that this one is a child of, and
    /// whether the toplevel is minimized.
    pub parent: Option<SurfaceId>,
    pub minimized: bool,
    /// Whether the pointer resizes the window, the size that a resize
    /// asks of the client, the serial of the configure with a size that the
    /// client has not answered with a commit yet and whether it
    /// acknowledged it, and the latest size asked since (MIKSPEC-0074.R30).
    pub resizing: bool,
    pub(super) outstanding: Option<u32>,
    pub(super) acked: bool,
    pub(super) latest: Option<(i32, i32)>,
}

#[cfg(test)]
impl XdgState {
    /// The state of a toplevel that has `parent` and may be minimized, for
    /// the tests of the window layer.
    pub(super) fn windowed(parent: Option<SurfaceId>, minimized: bool) -> XdgState {
        XdgState {
            parent,
            minimized,
            ..XdgState::default()
        }
    }
}

impl Server {
    pub(super) fn xdg_request(
        &mut self,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let decoded =
            |e: DecodeError| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match resource {
            Resource::WmBase => self.wm_base(
                request,
                xdg_wm_base::Request::decode(request.opcode, args).map_err(decoded)?,
            ),
            Resource::Positioner => self.positioner_request(request, args),
            Resource::XdgSurface(id) => self.xdg_surface(
                request,
                id,
                xdg_surface::Request::decode(request.opcode, args).map_err(decoded)?,
            ),
            Resource::Toplevel(id) => self.toplevel(
                request,
                id,
                xdg_toplevel::Request::decode(request.opcode, args).map_err(decoded)?,
            ),
            _ => unreachable!("xdg_request handles xdg-shell"),
        }
    }

    fn wm_base(
        &mut self,
        request: Request,
        decoded: xdg_wm_base::Request,
    ) -> Result<(), ProtocolError> {
        match decoded {
            xdg_wm_base::Request::Destroy => self.client_mut(request).destroy(request.object),
            xdg_wm_base::Request::CreatePositioner { id } => {
                let c = self.client_mut(request);
                c.create(
                    id,
                    &xdg_positioner::INTERFACE,
                    request.version,
                    Resource::Positioner,
                )?;
                self.new_positioner(request.client, id.0);
            }
            xdg_wm_base::Request::GetXdgSurface { id, surface } => {
                let c = self.client_mut(request);
                let Resource::Surface(surface_id) =
                    c.resource(request.object, surface, &wl_surface::INTERFACE)?
                else {
                    unreachable!("surfaces are wl_surface objects");
                };
                let role_error = |message: &str| {
                    ProtocolError::new(request.object, xdg_wm_base::error::ROLE, message)
                };
                let s = self.surface_mut(surface_id)?;
                if s.role != Role::None || s.xdg.is_some() {
                    return Err(role_error("the surface has a role"));
                }
                let waits = !s.waiting.is_empty();
                if s.current.buffer.is_some() || s.pending.buffer.is_some() || waits {
                    return Err(ProtocolError::new(
                        request.object,
                        xdg_wm_base::error::INVALID_SURFACE_STATE,
                        "the surface has a buffer",
                    ));
                }
                s.xdg = Some(XdgState {
                    object: id.0,
                    ..XdgState::default()
                });
                let c = self.client_mut(request);
                c.create(
                    id,
                    &xdg_surface::INTERFACE,
                    request.version,
                    Resource::XdgSurface(surface_id),
                )?;
            }
            xdg_wm_base::Request::Pong { .. } => {}
        }
        Ok(())
    }

    fn xdg_surface(
        &mut self,
        request: Request,
        id: SurfaceId,
        decoded: xdg_surface::Request,
    ) -> Result<(), ProtocolError> {
        let error = |code, message: &str| ProtocolError::new(request.object, code, message);
        let xdg = self.xdg_mut(id)?;
        match decoded {
            xdg_surface::Request::Destroy => {
                if xdg.toplevel.is_some() {
                    return Err(error(
                        xdg_surface::error::DEFUNCT_ROLE_OBJECT,
                        "the toplevel lives",
                    ));
                }
                self.surface_mut(id)?.xdg = None;
                self.client_mut(request).destroy(request.object);
            }
            xdg_surface::Request::GetToplevel { id: toplevel } => {
                if xdg.toplevel.is_some() {
                    return Err(error(
                        xdg_surface::error::ALREADY_CONSTRUCTED,
                        "a toplevel already",
                    ));
                }
                xdg.toplevel = Some(toplevel.0);
                self.surface_mut(id)?.role = Role::Toplevel;
                let c = self.client_mut(request);
                c.create(
                    toplevel,
                    &xdg_toplevel::INTERFACE,
                    request.version,
                    Resource::Toplevel(id),
                )?;
            }
            xdg_surface::Request::GetPopup {
                id: popup,
                parent,
                positioner,
            } => {
                if xdg.toplevel.is_some() || self.popups.popups.contains_key(&id) {
                    return Err(error(
                        xdg_surface::error::ALREADY_CONSTRUCTED,
                        "a role already",
                    ));
                }
                self.get_popup(request, id, popup, parent, positioner)?;
            }
            xdg_surface::Request::SetWindowGeometry {
                x,
                y,
                width,
                height,
            } => {
                if width <= 0 || height <= 0 {
                    return Err(error(xdg_surface::error::INVALID_SIZE, "an empty geometry"));
                }
                xdg.geometry = Some(super::Rect {
                    x,
                    y,
                    width,
                    height,
                });
            }
            xdg_surface::Request::AckConfigure { serial } => {
                let Some(at) = xdg.sent.iter().position(|&s| s == serial) else {
                    return Err(error(
                        xdg_surface::error::INVALID_SERIAL,
                        "no such configure",
                    ));
                };
                xdg.sent.drain(..=at);
                xdg.configured = true;
                if xdg.outstanding.is_some_and(|o| serial >= o) {
                    xdg.acked = true;
                }
            }
        }
        Ok(())
    }

    fn xdg_mut(&mut self, id: SurfaceId) -> Result<&mut XdgState, ProtocolError> {
        self.surface_mut(id)?
            .xdg
            .as_mut()
            .ok_or_else(|| ProtocolError::invalid_object("a destroyed xdg_surface"))
    }

    fn toplevel(
        &mut self,
        request: Request,
        id: SurfaceId,
        decoded: xdg_toplevel::Request,
    ) -> Result<(), ProtocolError> {
        let error = |message: &str| {
            ProtocolError::new(request.object, xdg_toplevel::error::INVALID_SIZE, message)
        };
        let invalid_parent = |message: &str| {
            ProtocolError::new(request.object, xdg_toplevel::error::INVALID_PARENT, message)
        };
        if let xdg_toplevel::Request::SetParent { parent } = decoded {
            let parent = match parent {
                None => None,
                Some(object) => {
                    match self
                        .client_mut(request)
                        .objects
                        .get(&object)
                        .map(|o| o.resource)
                    {
                        Some(Resource::Toplevel(surface)) => Some(surface),
                        _ => return Err(invalid_parent("a parent that is not a toplevel")),
                    }
                }
            };
            // A parent that has this toplevel among its ancestors would
            // make a ring.
            let mut ancestor = parent;
            while let Some(a) = ancestor {
                if a == id {
                    return Err(invalid_parent("a parent that is a descendant"));
                }
                ancestor = self.surfaces.get(&a).and_then(|s| s.xdg.as_ref()?.parent);
            }
            self.xdg_mut(id)?.parent = parent;
            return Ok(());
        }
        if let xdg_toplevel::Request::Move { serial, .. } = decoded {
            self.request_move(request.client, id, serial);
            return Ok(());
        }
        if let xdg_toplevel::Request::Resize { serial, edges, .. } = decoded {
            if !super::resize::valid_edges(edges) {
                return Err(ProtocolError::new(
                    request.object,
                    xdg_toplevel::error::INVALID_RESIZE_EDGE,
                    "an edge that xdg_toplevel does not name",
                ));
            }
            self.request_resize(request.client, id, serial, edges);
            return Ok(());
        }
        let xdg = self.xdg_mut(id)?;
        let mut changed = false;
        match decoded {
            xdg_toplevel::Request::Destroy => {
                xdg.toplevel = None;
                self.client_mut(request).destroy(request.object);
                return Ok(());
            }
            xdg_toplevel::Request::SetTitle { title } => xdg.title = title,
            xdg_toplevel::Request::SetAppId { app_id } => xdg.app_id = app_id,
            xdg_toplevel::Request::SetMaxSize { width, height } => {
                if width < 0 || height < 0 {
                    return Err(error("a negative maximum size"));
                }
                xdg.max_size = (width, height);
            }
            xdg_toplevel::Request::SetMinSize { width, height } => {
                if width < 0 || height < 0 {
                    return Err(error("a negative minimum size"));
                }
                xdg.min_size = (width, height);
            }
            xdg_toplevel::Request::SetMaximized => (xdg.maximized, changed) = (true, true),
            xdg_toplevel::Request::UnsetMaximized => (xdg.maximized, changed) = (false, true),
            xdg_toplevel::Request::SetFullscreen { .. } => (xdg.fullscreen, changed) = (true, true),
            xdg_toplevel::Request::UnsetFullscreen => (xdg.fullscreen, changed) = (false, true),
            xdg_toplevel::Request::SetMinimized => xdg.minimized = true,
            xdg_toplevel::Request::SetParent { .. } => unreachable!("handled before"),
            // Menus and resizes come with the rest of window management.
            xdg_toplevel::Request::Move { .. } => unreachable!("handled before"),
            xdg_toplevel::Request::Resize { .. } => unreachable!("handled before"),
            xdg_toplevel::Request::ShowWindowMenu { .. } => {}
        }
        if changed && xdg.initialized {
            self.configure_now(id, None);
        }
        Ok(())
    }

    /// Checks a commit of the surface `id` against its role.
    pub(super) fn check_commit(
        &mut self,
        request: Request,
        id: SurfaceId,
    ) -> Result<(), ProtocolError> {
        let orphan = self
            .popups
            .popups
            .get(&id)
            .is_some_and(|p| p.parent.is_none());
        let s = self.surface_mut(id)?;
        let Some(xdg) = &s.xdg else {
            return Ok(());
        };
        let error = |code, message: &str| ProtocolError::new(xdg.object, code, message);
        if xdg.toplevel.is_none() && s.role == Role::None {
            return Err(error(
                xdg_surface::error::NOT_CONSTRUCTED,
                "an xdg_surface without a role",
            ));
        }
        if !xdg.initialized && orphan {
            return Err(error(
                xdg_surface::error::NOT_CONSTRUCTED,
                "a popup without a parent at its first commit",
            ));
        }
        if s.pending.buffer.is_some() && !xdg.configured {
            return Err(error(
                xdg_surface::error::UNCONFIGURED_BUFFER,
                "a buffer before a configure",
            ));
        }
        let _ = request;
        Ok(())
    }

    /// Sends the first configure events of a toplevel after its initial
    /// commit.
    pub(super) fn committed(&mut self, client: super::ClientId, id: SurfaceId) {
        let Some(xdg) = self.surfaces.get_mut(&id).and_then(|s| s.xdg.as_mut()) else {
            return;
        };
        if xdg.toplevel.is_some() && !xdg.initialized {
            xdg.initialized = true;
            self.configure(client, id);
        } else if !xdg.initialized && self.popups.popups.contains_key(&id) {
            xdg.initialized = true;
            self.configure_popup(id);
        }
        self.resize_committed(id);
    }

    /// Sends a toplevel's state to its client: the size, which is the
    /// screen's for a fullscreen toplevel, the work area's for a maximized
    /// one and otherwise the client's choice, and its states, and then the serial to acknowledge.
    pub(super) fn configure(&mut self, client: super::ClientId, id: SurfaceId) -> Option<u32> {
        self.configure_with(client, id, None)
    }

    /// As `configure`, asking the client for `size` unless the window is
    /// fullscreen or maximized.
    pub(super) fn configure_with(
        &mut self,
        client: super::ClientId,
        id: SurfaceId,
        size: Option<(i32, i32)>,
    ) -> Option<u32> {
        let serial = self.next_serial();
        let screen = self.screen;
        let area = self.work_area();
        let xdg = self.surfaces.get_mut(&id).and_then(|s| s.xdg.as_mut())?;
        let toplevel = xdg.toplevel?;
        let mut states = vec![xdg_toplevel::state::ACTIVATED];
        let (mut width, mut height) = (0, 0);
        if xdg.fullscreen {
            states.push(xdg_toplevel::state::FULLSCREEN);
            (width, height) = screen.unwrap_or((0, 0));
        } else if xdg.maximized {
            states.push(xdg_toplevel::state::MAXIMIZED);
            (width, height) = (area.width, area.height);
        } else if let Some(size) = size {
            (width, height) = size;
        }
        if xdg.resizing {
            states.push(xdg_toplevel::state::RESIZING);
        }
        xdg.sent.push(serial);
        let object = xdg.object;
        let states: Vec<u8> = states.iter().flat_map(|s| s.to_ne_bytes()).collect();
        let c = self
            .clients
            .get_mut(&client)
            .expect("a client with surfaces");
        xdg_toplevel::send_configure(&mut c.out, toplevel, width, height, &states);
        xdg_surface::send_configure(&mut c.out, object, serial);
        Some(serial)
    }
}

#[cfg(test)]
mod tests {
    use super::super::panel::Edge;
    use super::super::surface::tests::with_surface;
    use super::super::tests::TestClient;
    use super::*;
    use crate::wayland::protocols::{wl_display, wl_registry};
    use crate::wayland::wire::NewId;

    /// A client with a surface 4 that is the toplevel 7 of the xdg_surface
    /// 6, with xdg_wm_base bound as 5.
    fn with_toplevel(server: &mut Server) -> TestClient {
        let client = with_surface(server);
        client.bind(server, 2, &xdg_wm_base::INTERFACE, 5);
        client.send(
            server,
            5,
            2,
            vec![Arg::NewId(NewId(6)), Arg::Object(Some(4))],
        );
        client.send(server, 6, 1, vec![Arg::NewId(NewId(7))]);
        client
    }

    fn events(client: &TestClient, server: &mut Server) -> Vec<(ObjectId, u16, Vec<Arg>)> {
        client.events(server, &|id| match id {
            1 => &wl_display::EVENTS,
            2 => &wl_registry::EVENTS,
            6 => &xdg_surface::EVENTS,
            _ => &xdg_toplevel::EVENTS,
        })
    }

    #[test]
    fn configures_a_toplevel_after_its_initial_commit() {
        let mut server = Server::new();
        server.screen = Some((1280, 800));
        let client = with_toplevel(&mut server);
        client.send(&mut server, 7, 11, vec![Arg::Object(None)]);
        events(&client, &mut server);
        client.send(&mut server, 4, 6, vec![]);
        let sent = events(&client, &mut server);
        let states: Vec<u8> = [4u32, 2].iter().flat_map(|s| s.to_ne_bytes()).collect();
        assert_eq!(
            sent[0],
            (
                7,
                0,
                vec![Arg::Int(1280), Arg::Int(800), Arg::Array(states)]
            )
        );
        let Arg::Uint(serial) = sent[1].2[0] else {
            panic!("{sent:?}")
        };
        assert_eq!((sent[1].0, sent[1].1), (6, 0));
        client.send(&mut server, 6, 4, vec![Arg::Uint(serial)]);
        assert_eq!(client.error(&mut server), None);
        let surface = server.surfaces().next().unwrap().1;
        assert_eq!(surface.role, Role::Toplevel);
        assert!(surface.xdg.as_ref().unwrap().configured);
    }

    #[test]
    fn refuses_a_parent_that_is_no_toplevel_or_makes_a_ring() {
        let mut server = Server::new();
        let client = with_toplevel(&mut server);
        client.send(&mut server, 7, 1, vec![Arg::Object(Some(7))]);
        let invalid = xdg_toplevel::error::INVALID_PARENT;
        assert_eq!(client.error(&mut server), Some(invalid));
        let client = with_toplevel(&mut server);
        client.send(&mut server, 7, 1, vec![Arg::Object(Some(6))]);
        assert_eq!(client.error(&mut server), Some(invalid));
    }

    #[test]
    fn refuses_a_parent_ring_of_two_toplevels() {
        let mut server = Server::new();
        let client = with_toplevel(&mut server);
        // A second toplevel, surface 8 with xdg_surface 9 and toplevel 10.
        client.send(&mut server, 3, 0, vec![Arg::NewId(NewId(8))]);
        client.send(
            &mut server,
            5,
            2,
            vec![Arg::NewId(NewId(9)), Arg::Object(Some(8))],
        );
        client.send(&mut server, 9, 1, vec![Arg::NewId(NewId(10))]);
        client.send(&mut server, 10, 1, vec![Arg::Object(Some(7))]);
        assert_eq!(client.error(&mut server), None);
        client.send(&mut server, 7, 1, vec![Arg::Object(Some(10))]);
        let invalid = xdg_toplevel::error::INVALID_PARENT;
        assert_eq!(client.error(&mut server), Some(invalid));
    }

    #[test]
    fn configures_a_maximized_toplevel_with_the_work_area() {
        let mut server = Server::new();
        server.screen = Some((1280, 800));
        super::super::windows::tests::panel(&mut server, 90, Edge::Bottom, 48, true);
        let client = with_toplevel(&mut server);
        client.send(&mut server, 7, 9, vec![]);
        events(&client, &mut server);
        client.send(&mut server, 4, 6, vec![]);
        let sent = events(&client, &mut server);
        let states: Vec<u8> = [4u32, 1].iter().flat_map(|s| s.to_ne_bytes()).collect();
        assert!(matches!(
            &sent[0].2[..],
            [Arg::Int(1280), Arg::Int(752), Arg::Array(a)] if *a == states
        ));
    }

    /// The `configure` events in `events`, as the size and the serial.
    fn configures(events: &[(ObjectId, u16, Vec<Arg>)]) -> Vec<((i32, i32), u32)> {
        let mut found = Vec::new();
        let mut size = (0, 0);
        for (object, opcode, args) in events {
            match (*object, *opcode, &args[..]) {
                (7, 0, [Arg::Int(w), Arg::Int(h), _]) => size = (*w, *h),
                (6, 0, [Arg::Uint(serial)]) => found.push((size, *serial)),
                _ => {}
            }
        }
        found
    }

    #[test]
    fn asks_for_one_size_at_a_time_until_the_client_answers_with_a_commit() {
        let mut server = Server::new();
        server.screen = Some((1280, 800));
        let client = with_toplevel(&mut server);
        client.send(&mut server, 4, 6, vec![]);
        let first = configures(&events(&client, &mut server));
        client.send(&mut server, 6, 4, vec![Arg::Uint(first[0].1)]);
        let surface = *server.surfaces().next().unwrap().0;
        server.surfaces.get_mut(&surface).unwrap().current.buffer = Some(99);
        // The first size goes out; the sizes after it wait.
        server.ask_size(surface, (300, 200));
        let sent = configures(&events(&client, &mut server));
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, (300, 200));
        server.ask_size(surface, (310, 210));
        server.ask_size(surface, (320, 220));
        assert!(events(&client, &mut server).is_empty());
        // An acknowledgement alone is not an answer.
        client.send(&mut server, 6, 4, vec![Arg::Uint(sent[0].1)]);
        assert!(configures(&events(&client, &mut server)).is_empty());
        // After a commit, the latest size goes out, and none between.
        client.send(&mut server, 4, 6, vec![]);
        let later = configures(&events(&client, &mut server));
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].0, (320, 220));
    }

    #[test]
    fn a_configure_sent_at_once_replaces_the_outstanding_one() {
        let mut server = Server::new();
        server.screen = Some((1280, 800));
        let client = with_toplevel(&mut server);
        client.send(&mut server, 4, 6, vec![]);
        let first = configures(&events(&client, &mut server));
        client.send(&mut server, 6, 4, vec![Arg::Uint(first[0].1)]);
        let surface = *server.surfaces().next().unwrap().0;
        server.surfaces.get_mut(&surface).unwrap().current.buffer = Some(99);
        let outstanding =
            |server: &Server| server.surfaces[&surface].xdg.as_ref().unwrap().outstanding;
        server.ask_size(surface, (300, 200));
        let older = configures(&events(&client, &mut server));
        assert_eq!(older.len(), 1);
        server.ask_size(surface, (310, 210));
        assert!(events(&client, &mut server).is_empty());
        // The final size goes out although the client has not answered, and
        // the size that waited is dropped.
        server.configure_now(surface, Some((320, 220)));
        let last = configures(&events(&client, &mut server));
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].0, (320, 220));
        assert_eq!(outstanding(&server), Some(last[0].1));
        // The answer to the older configure is not one to the last.
        client.send(&mut server, 6, 4, vec![Arg::Uint(older[0].1)]);
        client.send(&mut server, 4, 6, vec![]);
        assert!(configures(&events(&client, &mut server)).is_empty());
        assert_eq!(outstanding(&server), Some(last[0].1));
        // The answer to the last one ends the wait, and nothing else is sent.
        client.send(&mut server, 6, 4, vec![Arg::Uint(last[0].1)]);
        client.send(&mut server, 4, 6, vec![]);
        assert!(configures(&events(&client, &mut server)).is_empty());
        assert_eq!(outstanding(&server), None);
        server.ask_size(surface, (330, 230));
        let next = configures(&events(&client, &mut server));
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].0, (330, 230));
    }

    #[test]
    fn a_client_that_does_not_answer_is_waited_for_without_a_limit() {
        let mut server = Server::new();
        server.screen = Some((1280, 800));
        let client = with_toplevel(&mut server);
        client.send(&mut server, 4, 6, vec![]);
        let first = configures(&events(&client, &mut server));
        client.send(&mut server, 6, 4, vec![Arg::Uint(first[0].1)]);
        let surface = *server.surfaces().next().unwrap().0;
        server.surfaces.get_mut(&surface).unwrap().current.buffer = Some(99);
        server.ask_size(surface, (300, 200));
        assert_eq!(configures(&events(&client, &mut server)).len(), 1);
        server.ask_size(surface, (310, 210));
        assert!(events(&client, &mut server).is_empty());
        // Nothing is sent again, however long the client takes.
        server.ask_size(surface, (320, 220));
        assert!(events(&client, &mut server).is_empty());
    }

    #[test]
    fn maximizing_sends_its_configure_at_once_over_an_outstanding_one() {
        let mut server = Server::new();
        server.screen = Some((1280, 800));
        let client = with_toplevel(&mut server);
        client.send(&mut server, 4, 6, vec![]);
        let first = configures(&events(&client, &mut server));
        client.send(&mut server, 6, 4, vec![Arg::Uint(first[0].1)]);
        let surface = *server.surfaces().next().unwrap().0;
        server.surfaces.get_mut(&surface).unwrap().current.buffer = Some(99);
        server.ask_size(surface, (300, 200));
        let older = configures(&events(&client, &mut server));
        assert_eq!(older.len(), 1);
        server.maximize_window(surface);
        let sent = configures(&events(&client, &mut server));
        assert_eq!(sent.len(), 1);
        assert!(sent[0].1 > older[0].1);
        let xdg = server.surfaces[&surface].xdg.as_ref().unwrap();
        assert_eq!(xdg.outstanding, Some(sent[0].1));
    }

    #[test]
    fn the_end_of_a_resize_sends_the_final_size_without_the_resizing_state() {
        use super::super::resize::Resize;
        let mut server = Server::new();
        server.screen = Some((1280, 800));
        let client = with_toplevel(&mut server);
        client.send(&mut server, 4, 6, vec![]);
        let first = configures(&events(&client, &mut server));
        client.send(&mut server, 6, 4, vec![Arg::Uint(first[0].1)]);
        let surface = *server.surfaces().next().unwrap().0;
        server.surfaces.get_mut(&surface).unwrap().current.buffer = Some(99);
        server.buffer_sizes.insert(99, (100, 50));
        server.move_window(surface, 100, 100);
        let frame = server.windows().into_iter().next().unwrap().frame;
        server.pointer.resizing = Some(Resize {
            surface,
            edges: 8,
            frame,
            pointer: (200, 125),
            button: 0x110,
            asked: (frame.width, frame.height),
        });
        server
            .surfaces
            .get_mut(&surface)
            .unwrap()
            .xdg
            .as_mut()
            .unwrap()
            .resizing = true;
        // A size that the client does not answer is outstanding.
        server.ask_size(surface, (300, 200));
        assert_eq!(configures(&events(&client, &mut server)).len(), 1);
        assert!(server.end_resize(0x110));
        let all = events(&client, &mut server);
        let sent = configures(&all);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, (frame.width, frame.height));
        let Some((_, _, args)) = all.iter().find(|e| (e.0, e.1) == (7, 0)) else {
            panic!("a configure: {all:?}");
        };
        let Arg::Array(states) = &args[2] else {
            panic!("states");
        };
        let resizing = xdg_toplevel::state::RESIZING.to_ne_bytes();
        assert!(!states.chunks(4).any(|s| s == resizing));
    }

    #[test]
    fn a_resize_tells_the_client_it_is_resizing_and_the_end_clears_it() {
        let mut server = Server::new();
        server.screen = Some((1280, 800));
        let client = with_toplevel(&mut server);
        client.send(&mut server, 4, 6, vec![]);
        let first = configures(&events(&client, &mut server));
        client.send(&mut server, 6, 4, vec![Arg::Uint(first[0].1)]);
        let surface = *server.surfaces().next().unwrap().0;
        server.surfaces.get_mut(&surface).unwrap().current.buffer = Some(99);
        server
            .surfaces
            .get_mut(&surface)
            .unwrap()
            .xdg
            .as_mut()
            .unwrap()
            .resizing = true;
        server.ask_size(surface, (300, 200));
        let all = events(&client, &mut server);
        let Some((_, _, args)) = all.iter().find(|e| (e.0, e.1) == (7, 0)) else {
            panic!("a configure: {all:?}");
        };
        let Arg::Array(states) = &args[2] else {
            panic!("states");
        };
        let resizing = xdg_toplevel::state::RESIZING.to_ne_bytes();
        assert!(states.chunks(4).any(|s| s == resizing));
    }

    #[test]
    fn refuses_roles_twice_and_unknown_serials() {
        let mut server = Server::new();
        let client = with_toplevel(&mut server);
        client.send(
            &mut server,
            5,
            2,
            vec![Arg::NewId(NewId(8)), Arg::Object(Some(4))],
        );
        assert_eq!(client.error(&mut server), Some(xdg_wm_base::error::ROLE));
        let client = with_toplevel(&mut server);
        client.send(&mut server, 6, 1, vec![Arg::NewId(NewId(8))]);
        assert_eq!(
            client.error(&mut server),
            Some(xdg_surface::error::ALREADY_CONSTRUCTED)
        );
        let client = with_toplevel(&mut server);
        client.send(&mut server, 6, 4, vec![Arg::Uint(77)]);
        assert_eq!(
            client.error(&mut server),
            Some(xdg_surface::error::INVALID_SERIAL)
        );
        let client = with_toplevel(&mut server);
        client.send(&mut server, 6, 0, vec![]);
        assert_eq!(
            client.error(&mut server),
            Some(xdg_surface::error::DEFUNCT_ROLE_OBJECT)
        );
        // A commit of an xdg_surface without a role.
        let client = with_surface(&mut server);
        client.bind(&mut server, 2, &xdg_wm_base::INTERFACE, 5);
        client.send(
            &mut server,
            5,
            2,
            vec![Arg::NewId(NewId(6)), Arg::Object(Some(4))],
        );
        client.send(&mut server, 4, 6, vec![]);
        assert_eq!(
            client.error(&mut server),
            Some(xdg_surface::error::NOT_CONSTRUCTED)
        );
    }
}
