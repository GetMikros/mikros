//! `wp_viewporter` (MIKSPEC-0066.R16): a client crops the buffer of a
//! surface to a source rectangle and sizes the surface by a destination,
//! both applied at the surface's next commit. The size of the surface is
//! then its destination, or else the size of its source, or else its
//! buffer's.

use super::{ClientId, ProtocolError, Request, Resource, Server, Surface, SurfaceId};
use crate::wayland::protocols::{
    wl_output::transform as wl_output_transform, wl_surface, wp_viewport, wp_viewporter,
};
use crate::wayland::wire::Arg;
use std::collections::BTreeMap;

pub const VERSION: u32 = 1;

/// The crop and size that a client set: the source rectangle in 24.8
/// fixed-point surface coordinates, and the destination size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Viewport {
    pub source: Option<[i32; 4]>,
    pub destination: Option<(i32, i32)>,
}

/// The viewports of surfaces, by surface, with their client and object.
#[derive(Debug, Default)]
pub(super) struct Viewports {
    objects: BTreeMap<SurfaceId, (ClientId, u32)>,
}

impl Server {
    pub(super) fn viewport_request(
        &mut self,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let invalid = |e| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match resource {
            Resource::Viewporter => {
                match wp_viewporter::Request::decode(request.opcode, args).map_err(invalid)? {
                    wp_viewporter::Request::Destroy => {
                        self.client_mut(request).destroy(request.object);
                    }
                    wp_viewporter::Request::GetViewport { id, surface } => {
                        let c = self.client_mut(request);
                        let Resource::Surface(surface_id) =
                            c.resource(request.object, surface, &wl_surface::INTERFACE)?
                        else {
                            unreachable!("surfaces are wl_surface objects");
                        };
                        if self.viewports.objects.contains_key(&surface_id) {
                            return Err(ProtocolError::new(
                                request.object,
                                wp_viewporter::error::VIEWPORT_EXISTS,
                                "the surface has a viewport",
                            ));
                        }
                        let c = self.client_mut(request);
                        let interface = &wp_viewport::INTERFACE;
                        let resource = Resource::Viewport(surface_id);
                        c.create(id, interface, request.version, resource)?;
                        self.viewports
                            .objects
                            .insert(surface_id, (request.client, id.0));
                    }
                }
                Ok(())
            }
            Resource::Viewport(surface) => {
                let decoded =
                    wp_viewport::Request::decode(request.opcode, args).map_err(invalid)?;
                let bad = |code, message| ProtocolError::new(request.object, code, message);
                let own =
                    self.viewports.objects.get(&surface) == Some(&(request.client, request.object));
                match decoded {
                    wp_viewport::Request::Destroy => {
                        self.client_mut(request).destroy(request.object);
                        if own {
                            self.viewports.objects.remove(&surface);
                            if let Some(s) = self.surfaces.get_mut(&surface) {
                                s.pending.viewport = Viewport::default();
                            }
                        }
                    }
                    wp_viewport::Request::SetSource {
                        x,
                        y,
                        width,
                        height,
                    } => {
                        let Some(s) = self.surfaces.get_mut(&surface) else {
                            return Err(bad(wp_viewport::error::NO_SURFACE, "the surface is gone"));
                        };
                        let all_unset = [x.0, y.0, width.0, height.0] == [-256; 4];
                        s.pending.viewport.source = if all_unset {
                            None
                        } else if x.0 < 0 || y.0 < 0 || width.0 <= 0 || height.0 <= 0 {
                            return Err(bad(
                                wp_viewport::error::BAD_VALUE,
                                "a source with a negative origin or no area",
                            ));
                        } else {
                            Some([x.0, y.0, width.0, height.0])
                        };
                    }
                    wp_viewport::Request::SetDestination { width, height } => {
                        let Some(s) = self.surfaces.get_mut(&surface) else {
                            return Err(bad(wp_viewport::error::NO_SURFACE, "the surface is gone"));
                        };
                        s.pending.viewport.destination = if (width, height) == (-1, -1) {
                            None
                        } else if width <= 0 || height <= 0 {
                            return Err(bad(
                                wp_viewport::error::BAD_VALUE,
                                "a destination without area",
                            ));
                        } else {
                            Some((width, height))
                        };
                    }
                }
                Ok(())
            }
            _ => unreachable!("viewport_request handles viewports"),
        }
    }

    /// Forgets the viewports of `client`, which went.
    pub(super) fn forget_client_viewports(&mut self, client: ClientId) {
        self.viewports.objects.retain(|_, (c, _)| *c != client);
    }

    /// Checks the viewport that the surface's next commit applies against
    /// the buffer it will show.
    pub(super) fn check_viewport(&self, surface: SurfaceId) -> Result<(), ProtocolError> {
        let Some(s) = self.surfaces.get(&surface) else {
            return Ok(());
        };
        let Some(&(_, object)) = self.viewports.objects.get(&surface) else {
            return Ok(());
        };
        let viewport = s.pending.viewport;
        // The newest buffer that was attached, which a commit may have
        // staged, held or made wait, as well as shown.
        let buffer = if s.attached {
            s.pending.buffer
        } else {
            s.pending.buffer.or(s.current.buffer)
        };
        let Some(&(width, height)) = buffer.and_then(|b| self.buffer_sizes.get(&b)) else {
            return Ok(());
        };
        let bad = |code, message| ProtocolError::new(object, code, message);
        if let (Some([_, _, w, h]), None) = (viewport.source, viewport.destination)
            && (w % 256 != 0 || h % 256 != 0)
        {
            return Err(bad(
                wp_viewport::error::BAD_SIZE,
                "a source of fractional size and no destination",
            ));
        }
        if let Some([x, y, w, h]) = viewport.source {
            // The buffer in surface coordinates: turned by its transform
            // and divided by its scale. The compositor does not size or
            // draw surfaces by their buffer's scale or transform yet, so a
            // source is cropped from the buffer's own texels meanwhile.
            let turned = matches!(
                s.pending.transform,
                wl_output_transform::_90
                    | wl_output_transform::_270
                    | wl_output_transform::FLIPPED_90
                    | wl_output_transform::FLIPPED_270
            );
            let (width, height) = if turned {
                (height, width)
            } else {
                (width, height)
            };
            let scale = i64::from(s.pending.scale.max(1));
            let (bw, bh) = (
                i64::from(width) * 256 / scale,
                i64::from(height) * 256 / scale,
            );
            if i64::from(x) + i64::from(w) > bw || i64::from(y) + i64::from(h) > bh {
                return Err(bad(
                    wp_viewport::error::OUT_OF_BUFFER,
                    "a source that reaches past the buffer",
                ));
            }
        }
        Ok(())
    }

    /// The size of `surface` in surface coordinates: its viewport's
    /// destination, or its source's size, or its buffer's.
    pub(super) fn extent(&self, surface: &Surface) -> (i32, i32) {
        let Some(buffer) = surface.current.buffer else {
            return (0, 0);
        };
        let viewport = surface.current.viewport;
        match (viewport.destination, viewport.source) {
            (Some(destination), _) => destination,
            (None, Some([_, _, w, h])) => (w / 256, h / 256),
            (None, None) => self
                .buffer_sizes
                .get(&buffer)
                .map_or((0, 0), |&(w, h)| (w as i32, h as i32)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Role;
    use super::super::tests::TestClient;
    use super::*;
    use crate::wayland::protocols::{wl_compositor, wl_registry};
    use crate::wayland::wire::{Fixed, NewId};

    /// A client with the viewporter 3, the surface 4 as a toplevel of a
    /// 100 by 50 buffer, and the viewport 6.
    fn client(server: &mut Server) -> (TestClient, SurfaceId) {
        let client = TestClient::new(server);
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(server, 2, &wl_compositor::INTERFACE, 5);
        client.bind(server, 2, &wp_viewporter::INTERFACE, 3);
        client.send(server, 5, 0, vec![Arg::NewId(NewId(4))]);
        let (&id, surface) = server.surfaces.iter_mut().next().unwrap();
        surface.role = Role::Toplevel;
        surface.current.buffer = Some(99);
        server.buffer_sizes.insert(99, (100, 50));
        client.send(
            server,
            3,
            1,
            vec![Arg::NewId(NewId(6)), Arg::Object(Some(4))],
        );
        (client, id)
    }

    fn fixed(values: [i32; 4]) -> Vec<Arg> {
        Vec::from(values.map(|v| Arg::Fixed(Fixed(v * 256))))
    }

    fn size_of_window(server: &Server) -> (i32, i32) {
        let shown = server.shown();
        (shown[0].width, shown[0].height)
    }

    #[test]
    fn a_destination_sizes_the_surface_when_committed() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        let (client, _) = client(&mut server);
        assert_eq!(size_of_window(&server), (100, 50));
        client.send(&mut server, 6, 2, vec![Arg::Int(300), Arg::Int(150)]);
        assert_eq!(size_of_window(&server), (100, 50));
        client.send(&mut server, 4, 6, vec![]);
        assert_eq!(size_of_window(&server), (300, 150));
        assert_eq!(server.shown()[0].extent, (300, 150));
        // Unset with -1 by -1, and by destroying the viewport.
        client.send(&mut server, 6, 2, vec![Arg::Int(-1), Arg::Int(-1)]);
        client.send(&mut server, 4, 6, vec![]);
        assert_eq!(size_of_window(&server), (100, 50));
        client.send(&mut server, 6, 2, vec![Arg::Int(20), Arg::Int(10)]);
        client.send(&mut server, 6, 0, vec![]);
        client.send(&mut server, 4, 6, vec![]);
        assert_eq!(size_of_window(&server), (100, 50));
    }

    #[test]
    fn a_source_without_a_destination_sizes_the_surface_as_big_as_itself() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        let (client, _) = client(&mut server);
        client.send(&mut server, 6, 1, fixed([10, 5, 40, 20]));
        client.send(&mut server, 4, 6, vec![]);
        assert_eq!(size_of_window(&server), (40, 20));
        assert_eq!(server.shown()[0].source, Some([2560, 1280, 10240, 5120]));
        // With a destination, that is the size.
        client.send(&mut server, 6, 2, vec![Arg::Int(80), Arg::Int(40)]);
        client.send(&mut server, 4, 6, vec![]);
        assert_eq!(size_of_window(&server), (80, 40));
    }

    #[test]
    fn rejects_values_that_make_no_sense() {
        let mut server = Server::new();
        let (client, _) = client(&mut server);
        let code = |server: &mut Server| client.error(server);
        client.send(&mut server, 6, 2, vec![Arg::Int(0), Arg::Int(10)]);
        assert_eq!(code(&mut server), Some(wp_viewport::error::BAD_VALUE));
        let mut server = Server::new();
        let (client, _) = self::client(&mut server);
        client.send(&mut server, 6, 1, fixed([-5, 0, 10, 10]));
        assert_eq!(
            client.error(&mut server),
            Some(wp_viewport::error::BAD_VALUE)
        );
        // A source that reaches past the 100 by 50 buffer, at the commit.
        let mut server = Server::new();
        let (client, _) = self::client(&mut server);
        client.send(&mut server, 6, 1, fixed([60, 0, 50, 10]));
        client.send(&mut server, 4, 6, vec![]);
        assert_eq!(
            client.error(&mut server),
            Some(wp_viewport::error::OUT_OF_BUFFER)
        );
        // A source of a fraction of a pixel and no destination.
        let mut server = Server::new();
        let (client, _) = self::client(&mut server);
        let half = vec![
            Arg::Fixed(Fixed(0)),
            Arg::Fixed(Fixed(0)),
            Arg::Fixed(Fixed(128)),
            Arg::Fixed(Fixed(256)),
        ];
        client.send(&mut server, 6, 1, half);
        client.send(&mut server, 4, 6, vec![]);
        assert_eq!(
            client.error(&mut server),
            Some(wp_viewport::error::BAD_SIZE)
        );
    }

    #[test]
    fn a_surface_has_one_viewport_at_a_time() {
        let mut server = Server::new();
        let (client, _) = client(&mut server);
        let get = |id| vec![Arg::NewId(NewId(id)), Arg::Object(Some(4))];
        client.send(&mut server, 6, 0, vec![]);
        client.send(&mut server, 3, 1, get(7));
        assert_eq!(client.error(&mut server), None);
        client.send(&mut server, 3, 1, get(8));
        let exists = wp_viewporter::error::VIEWPORT_EXISTS;
        assert_eq!(client.error(&mut server), Some(exists));
        let _ = wl_registry::EVENTS;
    }
}
