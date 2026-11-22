//! `wp_fractional_scale_manager_v1` (MIKSPEC-0066.R16): a client gets the
//! scale of the output its surface is on as `preferred_scale`, in 120ths,
//! when it asks for the surface's object. The output has the scale of
//! `OUTPUT_SCALE` until outputs have configured scales (MIKSPEC-0066.R32).

use super::{ClientId, ProtocolError, Request, Resource, Server, SurfaceId};
use crate::wayland::protocols::{
    wl_surface, wp_fractional_scale_manager_v1, wp_fractional_scale_v1,
};
use crate::wayland::wire::{Arg, ObjectId};
use std::collections::BTreeMap;

pub const VERSION: u32 = 1;
/// The scale of the output in percent.
const OUTPUT_SCALE: u32 = 100;

/// The fractional scale objects of surfaces, by surface.
#[derive(Debug, Default)]
pub(super) struct Fractional {
    objects: BTreeMap<SurfaceId, (ClientId, ObjectId)>,
}

/// `preferred_scale` of an output of `percent`.
fn preferred(percent: u32) -> u32 {
    percent * 120 / 100
}

impl Server {
    pub(super) fn fractional_request(
        &mut self,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let invalid = |e| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match resource {
            Resource::FractionalManager => {
                match wp_fractional_scale_manager_v1::Request::decode(request.opcode, args)
                    .map_err(invalid)?
                {
                    wp_fractional_scale_manager_v1::Request::Destroy => {
                        self.client_mut(request).destroy(request.object);
                    }
                    wp_fractional_scale_manager_v1::Request::GetFractionalScale { id, surface } => {
                        let c = self.client_mut(request);
                        let Resource::Surface(surface_id) =
                            c.resource(request.object, surface, &wl_surface::INTERFACE)?
                        else {
                            unreachable!("surfaces are wl_surface objects");
                        };
                        if self.fractional.objects.contains_key(&surface_id) {
                            return Err(ProtocolError::new(
                                request.object,
                                wp_fractional_scale_manager_v1::error::FRACTIONAL_SCALE_EXISTS,
                                "the surface has a fractional scale object",
                            ));
                        }
                        let c = self.client_mut(request);
                        let interface = &wp_fractional_scale_v1::INTERFACE;
                        let resource = Resource::FractionalScale(surface_id);
                        c.create(id, interface, request.version, resource)?;
                        wp_fractional_scale_v1::send_preferred_scale(
                            &mut c.out,
                            id.0,
                            preferred(OUTPUT_SCALE),
                        );
                        self.fractional
                            .objects
                            .insert(surface_id, (request.client, id.0));
                    }
                }
                Ok(())
            }
            Resource::FractionalScale(surface) => {
                match wp_fractional_scale_v1::Request::decode(request.opcode, args)
                    .map_err(invalid)?
                {
                    wp_fractional_scale_v1::Request::Destroy => {
                        self.client_mut(request).destroy(request.object);
                        if self.fractional.objects.get(&surface)
                            == Some(&(request.client, request.object))
                        {
                            self.fractional.objects.remove(&surface);
                        }
                    }
                }
                Ok(())
            }
            _ => unreachable!("fractional_request handles fractional scale"),
        }
    }

    /// Forgets the fractional scale objects of `client`, which went.
    pub(super) fn forget_client_fractional(&mut self, client: ClientId) {
        self.fractional.objects.retain(|_, (c, _)| *c != client);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestClient;
    use super::*;
    use crate::wayland::protocols::{wl_compositor, wl_registry};
    use crate::wayland::wire::NewId;

    /// A client with the manager 3 and the surface 4.
    fn client(server: &mut Server) -> TestClient {
        let client = TestClient::new(server);
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(server, 2, &wl_compositor::INTERFACE, 5);
        client.bind(server, 2, &wp_fractional_scale_manager_v1::INTERFACE, 3);
        client.send(server, 5, 0, vec![Arg::NewId(NewId(4))]);
        client
    }

    fn events(server: &mut Server, client: &TestClient) -> Vec<(ObjectId, u16, Vec<Arg>)> {
        client.events(server, &|id| match id {
            6 => &wp_fractional_scale_v1::EVENTS,
            _ => &wl_registry::EVENTS,
        })
    }

    #[test]
    fn tells_the_scale_of_the_output_in_120ths_when_the_object_is_made() {
        let mut server = Server::new();
        let client = client(&mut server);
        client.send(
            &mut server,
            3,
            1,
            vec![Arg::NewId(NewId(6)), Arg::Object(Some(4))],
        );
        let sent = events(&mut server, &client);
        assert!(sent.contains(&(6, 0, vec![Arg::Uint(120)])), "{sent:?}");
        assert_eq!(preferred(150), 180);
        assert_eq!(preferred(275), 330);
    }

    #[test]
    fn a_surface_has_one_object_at_a_time() {
        let mut server = Server::new();
        let client = client(&mut server);
        let get = |id| vec![Arg::NewId(NewId(id)), Arg::Object(Some(4))];
        client.send(&mut server, 3, 1, get(6));
        // Destroying the object lets the surface have another.
        client.send(&mut server, 6, 0, vec![]);
        client.send(&mut server, 3, 1, get(7));
        assert_eq!(client.error(&mut server), None);
        client.send(&mut server, 3, 1, get(8));
        let exists = wp_fractional_scale_manager_v1::error::FRACTIONAL_SCALE_EXISTS;
        assert_eq!(client.error(&mut server), Some(exists));
    }
}
