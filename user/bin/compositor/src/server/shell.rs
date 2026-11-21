//! The output and the shell global. `wl_output` describes the one output
//! that the compositor shows; `mikros_shell_v1` is offered only on the
//! connections of the shell's components, each with its roles
//! (MIKSPEC-0076.R12). Of the shell's requests, `set_wallpaper` and
//! `get_panel` are served yet: the first gives the renderer the buffer's
//! pixels as the output's wallpaper (MIKSPEC-0075.R22) and then releases
//! the buffer, the second makes a panel (MIKSPEC-0076.R3) of a surface
//! without a role. The others end the component's connection with
//! wl_display's `implementation` error until they are served.

use super::{Imports, ProtocolError, Request, Resource, Server};
use crate::wayland::protocols::{
    mikros_panel_v1, mikros_shell_v1, wl_buffer, wl_display, wl_output, wl_surface,
};
use crate::wayland::wire::{Arg, DecodeError};

/// The protocol error of a request of the shell global that its
/// component may not make, for which the protocol file defines no code.
pub(super) const SHELL_ERROR: u32 = 0;

impl Server {
    /// Sends the description of the output to `output`, a new object of
    /// `version`, of `client`.
    pub(super) fn describe_output(&mut self, client: super::ClientId, output: u32, version: u32) {
        let (width, height) = self.screen.unwrap_or((0, 0));
        let Some(c) = self.clients.get_mut(&client) else {
            return;
        };
        wl_output::send_geometry(
            &mut c.out,
            output,
            0,
            0,
            0,
            0,
            wl_output::subpixel::UNKNOWN as i32,
            "",
            "",
            wl_output::transform::NORMAL as i32,
        );
        let current = wl_output::mode::CURRENT | wl_output::mode::PREFERRED;
        wl_output::send_mode(&mut c.out, output, current, width, height, 60_000);
        if version >= 2 {
            wl_output::send_scale(&mut c.out, output, 1);
            wl_output::send_done(&mut c.out, output);
        }
    }

    pub(super) fn output_request(
        &mut self,
        request: Request,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let decoded = wl_output::Request::decode(request.opcode, args)
            .map_err(|e| ProtocolError::invalid_method(request.object, format!("{e:?}")))?;
        match decoded {
            wl_output::Request::Release => self.client_mut(request).destroy(request.object),
        }
        Ok(())
    }

    pub(super) fn shell_request(
        &mut self,
        imports: &mut dyn Imports,
        request: Request,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let decoded =
            mikros_shell_v1::Request::decode(request.opcode, args).map_err(|e: DecodeError| {
                ProtocolError::invalid_method(request.object, format!("{e:?}"))
            })?;
        let error = |message: &str| ProtocolError::new(request.object, SHELL_ERROR, message);
        match decoded {
            mikros_shell_v1::Request::SetWallpaper { output, buffer } => {
                if !self.client_mut(request).has_role("desktop") {
                    return Err(error("set_wallpaper needs the role desktop"));
                }
                let c = self.client_mut(request);
                c.resource(request.object, output, &wl_output::INTERFACE)?;
                let Resource::Buffer(id) =
                    c.resource(request.object, buffer, &wl_buffer::INTERFACE)?
                else {
                    unreachable!("buffers are wl_buffer objects");
                };
                if !imports.set_wallpaper(id) {
                    return Err(error("the wallpaper's buffer cannot be copied"));
                }
                wl_buffer::send_release(&mut self.client_mut(request).out, buffer);
                Ok(())
            }
            mikros_shell_v1::Request::GetPanel {
                id,
                surface,
                output,
                edge,
            } => {
                let c = self.client_mut(request);
                if !c.has_role("menubar") && !c.has_role("taskbar") {
                    return Err(error("get_panel needs the role menubar or taskbar"));
                }
                c.resource(request.object, output, &wl_output::INTERFACE)?;
                let Resource::Surface(surface) =
                    c.resource(request.object, surface, &wl_surface::INTERFACE)?
                else {
                    unreachable!("surfaces are wl_surface objects");
                };
                let interface = &mikros_panel_v1::INTERFACE;
                c.create(id, interface, request.version, Resource::Panel(surface))?;
                self.make_panel(request, id.0, surface, edge)
            }
            // Requests that are not served yet end the connection with
            // wl_display's error for that, rather than doing nothing.
            _ => Err(ProtocolError::new(
                request.object,
                wl_display::error::IMPLEMENTATION,
                "a request of the shell global that is not served yet",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestClient;
    use super::*;
    use crate::wayland::protocols::{mikros_buffer_v1, wl_registry};
    use crate::wayland::wire::{Fd, NewId};

    /// The connection of a shell component of `roles`.
    fn component(server: &mut Server, roles: &[&str]) -> TestClient {
        let roles = roles.iter().map(|r| r.to_string()).collect();
        TestClient {
            id: server.connect_component(roles),
            imports: Default::default(),
        }
    }

    /// The names of the globals that `client` sees through its registry 2.
    fn globals(server: &mut Server, client: &TestClient) -> Vec<String> {
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        let events = client.events(server, &|_| &wl_registry::EVENTS);
        events
            .iter()
            .filter_map(|e| match &e.2[1] {
                Arg::String(Some(name)) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Binds the output as 3, the buffer global as 4 with the buffer 5,
    /// and the shell global as 6, and sends `set_wallpaper` of 3 and 5.
    fn set_wallpaper(server: &mut Server, client: &TestClient) {
        client.bind(server, 2, &wl_output::INTERFACE, 3);
        client.bind(server, 2, &mikros_buffer_v1::INTERFACE, 4);
        let mut token = vec![Arg::NewId(NewId(5)), Arg::Fd(Fd(40))];
        token.extend([64, 32, 0x3432_5258, 0, 0, 256, 0].map(Arg::Uint));
        client.send(server, 4, 0, token);
        client.bind(server, 2, &mikros_shell_v1::INTERFACE, 6);
        let args = vec![Arg::Object(Some(3)), Arg::Object(Some(5))];
        client.send(server, 6, 6, args);
    }

    #[test]
    fn offers_the_shell_global_to_components_only() {
        let mut server = Server::new();
        let app = TestClient::new(&mut server);
        assert!(!globals(&mut server, &app).contains(&"mikros_shell_v1".into()));
        let desktop = component(&mut server, &["desktop"]);
        let names = globals(&mut server, &desktop);
        assert!(names.contains(&"mikros_shell_v1".into()));
        assert!(names.contains(&"wl_output".into()));
    }

    #[test]
    fn copies_the_wallpaper_of_the_desktop_and_releases_its_buffer() {
        let mut server = Server::new();
        server.screen = Some((1280, 800));
        let desktop = component(&mut server, &["desktop"]);
        globals(&mut server, &desktop);
        set_wallpaper(&mut server, &desktop);
        assert_eq!(desktop.error(&mut server), None);
        assert_eq!(desktop.imports.borrow().wallpapers.len(), 1);
        let events = desktop.events(&mut server, &|id| match id {
            3 => &wl_output::EVENTS,
            4 => &mikros_buffer_v1::EVENTS,
            _ => &wl_buffer::EVENTS,
        });
        let mode = events.iter().find(|e| e.0 == 3 && e.1 == 1).unwrap();
        assert_eq!(mode.2[1..3], [Arg::Int(1280), Arg::Int(800)]);
        assert!(events.iter().any(|e| (e.0, e.1) == (5, 0)));
    }

    #[test]
    fn ends_requests_that_are_not_served_with_the_implementation_error() {
        let mut server = Server::new();
        let shell = component(&mut server, &["menubar"]);
        globals(&mut server, &shell);
        shell.bind(&mut server, 2, &mikros_shell_v1::INTERFACE, 3);
        // logout, request 8, which the compositor does not serve yet.
        shell.send(&mut server, 3, 8, vec![]);
        assert_eq!(
            shell.error(&mut server),
            Some(wl_display::error::IMPLEMENTATION)
        );
    }

    #[test]
    fn refuses_wallpapers_from_other_roles() {
        let mut server = Server::new();
        let taskbar = component(&mut server, &["taskbar"]);
        globals(&mut server, &taskbar);
        set_wallpaper(&mut server, &taskbar);
        assert_eq!(taskbar.error(&mut server), Some(SHELL_ERROR));
        assert!(taskbar.imports.borrow().wallpapers.is_empty());
    }
}
