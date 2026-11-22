//! `zwp_keyboard_shortcuts_inhibit_manager_v1` (MIKSPEC-0066.R16): a
//! client's inhibitor for a surface is active while that surface has the
//! keyboard focus, and then the compositor's shortcuts
//! (MIKSPEC-0074.R13) are the client's keys. The key that switches to
//! another screen stays the compositor's (MIKSPEC-0066.R30).

use super::{ClientId, ProtocolError, Request, Resource, Server, SurfaceId};
use crate::wayland::protocols::{
    wl_seat, wl_surface, zwp_keyboard_shortcuts_inhibit_manager_v1 as manager,
    zwp_keyboard_shortcuts_inhibitor_v1 as inhibitor,
};
use crate::wayland::wire::{Arg, ObjectId};
use std::collections::BTreeMap;

pub const VERSION: u32 = 1;

/// The inhibitors of surfaces, by surface, with their client and object.
#[derive(Debug, Default)]
pub(super) struct Inhibitors {
    objects: BTreeMap<SurfaceId, (ClientId, ObjectId)>,
}

impl Server {
    pub(super) fn inhibit_request(
        &mut self,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let invalid = |e| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match resource {
            Resource::InhibitManager => {
                match manager::Request::decode(request.opcode, args).map_err(invalid)? {
                    manager::Request::Destroy => {
                        self.client_mut(request).destroy(request.object);
                    }
                    manager::Request::InhibitShortcuts { id, surface, seat } => {
                        let c = self.client_mut(request);
                        let Resource::Surface(surface_id) =
                            c.resource(request.object, surface, &wl_surface::INTERFACE)?
                        else {
                            unreachable!("surfaces are wl_surface objects");
                        };
                        c.resource(request.object, seat, &wl_seat::INTERFACE)?;
                        if self.inhibitors.objects.contains_key(&surface_id) {
                            return Err(ProtocolError::new(
                                request.object,
                                manager::error::ALREADY_INHIBITED,
                                "the surface has an inhibitor for the seat",
                            ));
                        }
                        let c = self.client_mut(request);
                        let interface = &inhibitor::INTERFACE;
                        let resource = Resource::Inhibitor(surface_id);
                        c.create(id, interface, request.version, resource)?;
                        self.inhibitors
                            .objects
                            .insert(surface_id, (request.client, id.0));
                        if self.keyboard_focus() == Some(surface_id) {
                            let c = self.client_mut(request);
                            inhibitor::send_active(&mut c.out, id.0);
                        }
                    }
                }
                Ok(())
            }
            Resource::Inhibitor(surface) => {
                match inhibitor::Request::decode(request.opcode, args).map_err(invalid)? {
                    inhibitor::Request::Destroy => {
                        self.client_mut(request).destroy(request.object);
                        if self.inhibitors.objects.get(&surface)
                            == Some(&(request.client, request.object))
                        {
                            self.inhibitors.objects.remove(&surface);
                        }
                    }
                }
                Ok(())
            }
            _ => unreachable!("inhibit_request handles shortcut inhibition"),
        }
    }

    /// Whether the window with the keyboard focus has an inhibitor, which
    /// is active while it has the focus.
    pub(super) fn shortcuts_inhibited(&self) -> bool {
        self.keyboard_focus()
            .is_some_and(|s| self.inhibitors.objects.contains_key(&s))
    }

    /// Tells the inhibitor of `surface`, if it has one, that it became
    /// active or inactive.
    pub(super) fn tell_inhibitor(&mut self, surface: SurfaceId, active: bool) {
        let Some(&(client, object)) = self.inhibitors.objects.get(&surface) else {
            return;
        };
        if let Some(c) = self.clients.get_mut(&client) {
            if active {
                inhibitor::send_active(&mut c.out, object);
            } else {
                inhibitor::send_inactive(&mut c.out, object);
            }
        }
    }

    /// Forgets the inhibitors of `client`, which went.
    pub(super) fn forget_client_inhibitors(&mut self, client: ClientId) {
        self.inhibitors.objects.retain(|_, (c, _)| *c != client);
    }
}

#[cfg(test)]
mod tests {
    use super::super::Role;
    use super::super::tests::TestClient;
    use super::super::xdg::XdgState;
    use super::*;
    use crate::wayland::protocols::{wl_callback, wl_compositor, wl_display, wl_registry};
    use crate::wayland::wire::NewId;

    const SUPER: u32 = 125;
    const UP: u32 = 103;

    /// A client with the manager 4, the seat 5 and the window `id`, a
    /// toplevel of the surface 6 with the keyboard focus.
    fn setup() -> (Server, TestClient, SurfaceId) {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        let client = TestClient::new(&mut server);
        client.send(&mut server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(&mut server, 2, &manager::INTERFACE, 4);
        client.bind(&mut server, 2, &wl_seat::INTERFACE, 5);
        client.bind(&mut server, 2, &wl_compositor::INTERFACE, 3);
        client.send(&mut server, 3, 0, vec![Arg::NewId(NewId(6))]);
        let (&id, surface) = server.surfaces.iter_mut().next().unwrap();
        surface.role = Role::Toplevel;
        surface.current.buffer = Some(99);
        surface.xdg = Some(XdgState::windowed(None, false));
        server.buffer_sizes.insert(99, (100, 50));
        (server, client, id)
    }

    fn inhibit(server: &mut Server, client: &TestClient, id: u32) {
        let args = vec![
            Arg::NewId(NewId(id)),
            Arg::Object(Some(6)),
            Arg::Object(Some(5)),
        ];
        client.send(server, 4, 1, args);
    }

    fn events(server: &mut Server, client: &TestClient) -> Vec<(ObjectId, u16)> {
        client
            .events(server, &|id| match id {
                1 => &wl_display::EVENTS,
                2 => &wl_registry::EVENTS,
                5 => &wl_seat::EVENTS,
                7 => &inhibitor::EVENTS,
                _ => &wl_callback::EVENTS,
            })
            .into_iter()
            .filter(|e| e.0 == 7)
            .map(|e| (e.0, e.1))
            .collect()
    }

    fn press_super_up(server: &mut Server) {
        server.keyboard_key(SUPER, true, 0, Default::default());
        server.keyboard_key(UP, true, 0, Default::default());
        server.keyboard_key(UP, false, 0, Default::default());
        server.keyboard_key(SUPER, false, 0, Default::default());
    }

    fn maximized(server: &Server, id: SurfaceId) -> bool {
        server.surfaces[&id].xdg.as_ref().unwrap().maximized
    }

    #[test]
    fn the_inhibitor_is_active_while_its_surface_has_the_keyboard() {
        let (mut server, client, id) = setup();
        inhibit(&mut server, &client, 7);
        assert!(events(&mut server, &client).is_empty());
        server.focus_keyboard(Some(id));
        assert_eq!(events(&mut server, &client), [(7, 0)]);
        server.focus_keyboard(None);
        assert_eq!(events(&mut server, &client), [(7, 1)]);
        // Made while the surface has the keyboard, it is active at once.
        let (mut server, client, id) = setup();
        server.focus_keyboard(Some(id));
        inhibit(&mut server, &client, 7);
        assert_eq!(events(&mut server, &client), [(7, 0)]);
    }

    #[test]
    fn shortcuts_are_the_clients_while_the_inhibitor_is_active() {
        let (mut server, client, id) = setup();
        server.focus_keyboard(Some(id));
        inhibit(&mut server, &client, 7);
        press_super_up(&mut server);
        assert!(!maximized(&server, id));
        // Destroyed, the shortcut is the compositor's again.
        client.send(&mut server, 7, 0, vec![]);
        press_super_up(&mut server);
        assert!(maximized(&server, id));
    }

    #[test]
    fn a_surface_has_one_inhibitor_at_a_time() {
        let (mut server, client, _) = setup();
        inhibit(&mut server, &client, 7);
        inhibit(&mut server, &client, 8);
        let code = manager::error::ALREADY_INHIBITED;
        assert_eq!(client.error(&mut server), Some(code));
    }
}
