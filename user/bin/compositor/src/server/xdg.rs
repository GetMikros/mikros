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
    sent: Vec<u32>,
    /// Whether the client acknowledged a configure event, and so may
    /// commit a buffer.
    pub configured: bool,
    /// Whether the initial commit was made, after which every change of
    /// the toplevel's state is configured.
    initialized: bool,
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
            Resource::Positioner => {
                if let xdg_positioner::Request::Destroy =
                    xdg_positioner::Request::decode(request.opcode, args).map_err(decoded)?
                {
                    self.client_mut(request).destroy(request.object);
                }
                Ok(())
            }
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
            xdg_surface::Request::GetPopup { .. } => {
                return Err(ProtocolError::new(
                    request.object,
                    crate::wayland::protocols::wl_display::error::IMPLEMENTATION,
                    "popups are not served yet",
                ));
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
                        _ => return Err(error("a parent that is not a toplevel")),
                    }
                }
            };
            // A parent that has this toplevel among its ancestors would
            // make a ring.
            let mut ancestor = parent;
            while let Some(a) = ancestor {
                if a == id {
                    return Err(error("a parent that is a descendant"));
                }
                ancestor = self.surfaces.get(&a).and_then(|s| s.xdg.as_ref()?.parent);
            }
            self.xdg_mut(id)?.parent = parent;
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
            // Menus, moves and resizes come with the rest of window
            // management.
            xdg_toplevel::Request::ShowWindowMenu { .. }
            | xdg_toplevel::Request::Move { .. }
            | xdg_toplevel::Request::Resize { .. } => {}
        }
        if changed && xdg.initialized {
            self.configure(request.client, id);
        }
        Ok(())
    }

    /// Checks a commit of the surface `id` against its role.
    pub(super) fn check_commit(
        &mut self,
        request: Request,
        id: SurfaceId,
    ) -> Result<(), ProtocolError> {
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
    pub(super) fn committed(&mut self, request: Request, id: SurfaceId) {
        let Some(xdg) = self.surfaces.get_mut(&id).and_then(|s| s.xdg.as_mut()) else {
            return;
        };
        if xdg.toplevel.is_some() && !xdg.initialized {
            xdg.initialized = true;
            self.configure(request.client, id);
        }
    }

    /// Sends a toplevel's state to its client: the size, which is the
    /// screen's for a fullscreen toplevel and otherwise the client's
    /// choice, and its states, and then the serial to acknowledge.
    fn configure(&mut self, client: super::ClientId, id: SurfaceId) {
        let serial = self.next_serial();
        let screen = self.screen;
        let Some(xdg) = self.surfaces.get_mut(&id).and_then(|s| s.xdg.as_mut()) else {
            return;
        };
        let Some(toplevel) = xdg.toplevel else {
            return;
        };
        let mut states = vec![xdg_toplevel::state::ACTIVATED];
        let (mut width, mut height) = (0, 0);
        if xdg.fullscreen {
            states.push(xdg_toplevel::state::FULLSCREEN);
            (width, height) = screen.unwrap_or((0, 0));
        } else if xdg.maximized {
            states.push(xdg_toplevel::state::MAXIMIZED);
            (width, height) = screen.unwrap_or((0, 0));
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
    }
}

#[cfg(test)]
mod tests {
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
