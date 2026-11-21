//! `mikros_material_v1` (MIKSPEC-0076.R16): the areas of a surface under
//! which the compositor draws a material (MIKSPEC-0066.R18). Each
//! `set_region` replaces the area of its material in the surface's pending
//! state and cuts it out of the other material's area; the next commit
//! applies both. A surface has one material object at a time, and the
//! errors are those of MIKSPEC-0076.R22.

use super::area::Area;
use super::{ProtocolError, Request, Resource, Server, SurfaceId};
use crate::wayland::protocols::{
    mikros_material_v1, mikros_surface_material_v1, wl_region, wl_surface,
};
use crate::wayland::wire::Arg;

const NONE: u32 = 0;
const HAZE: u32 = 1;
const BLUR: u32 = 2;

/// The areas of a surface's materials, in surface coordinates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Materials {
    pub haze: Area,
    pub blur: Area,
}

impl Server {
    pub(super) fn material_request(
        &mut self,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let invalid = |e| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match resource {
            Resource::Materials => {
                let mikros_material_v1::Request::GetMaterial { id, surface } =
                    mikros_material_v1::Request::decode(request.opcode, args).map_err(invalid)?;
                let c = self.client_mut(request);
                let Resource::Surface(surface) =
                    c.resource(request.object, surface, &wl_surface::INTERFACE)?
                else {
                    unreachable!("surfaces are wl_surface objects");
                };
                if self.surface_mut(surface)?.material.is_some() {
                    return Err(ProtocolError::new(
                        request.object,
                        mikros_material_v1::error::SURFACE_EXISTS,
                        "a second material object of a surface",
                    ));
                }
                let interface = &mikros_surface_material_v1::INTERFACE;
                let c = self.client_mut(request);
                c.create(id, interface, request.version, Resource::Material(surface))?;
                self.surface_mut(surface)?.material = Some(id.0);
                Ok(())
            }
            Resource::Material(surface) => {
                match mikros_surface_material_v1::Request::decode(request.opcode, args)
                    .map_err(invalid)?
                {
                    mikros_surface_material_v1::Request::SetRegion { material, region } => {
                        self.set_material(request, surface, material, region)
                    }
                    mikros_surface_material_v1::Request::Destroy => {
                        if let Some(s) = self.surfaces.get_mut(&surface)
                            && s.material == Some(request.object)
                        {
                            s.material = None;
                        }
                        self.client_mut(request).destroy(request.object);
                        Ok(())
                    }
                }
            }
            _ => unreachable!("material_request handles materials"),
        }
    }

    fn set_material(
        &mut self,
        request: Request,
        surface: SurfaceId,
        material: u32,
        region: u32,
    ) -> Result<(), ProtocolError> {
        let error = |code, message: &str| ProtocolError::new(request.object, code, message);
        if material > BLUR {
            let code = mikros_surface_material_v1::error::INVALID_MATERIAL;
            return Err(error(code, "a material that is none, haze or blur"));
        }
        self.client_mut(request)
            .resource(request.object, region, &wl_region::INTERFACE)?;
        let rects = self.regions.get(&(request.client, region));
        let area = Area::of(rects.map(Vec::as_slice).unwrap_or_default());
        let Some(surface) = self.surfaces.get_mut(&surface) else {
            let code = mikros_surface_material_v1::error::NO_SURFACE;
            return Err(error(code, "a request of a material whose surface is gone"));
        };
        let materials = &mut surface.pending.materials;
        materials.haze.cut(&area);
        materials.blur.cut(&area);
        match material {
            HAZE => materials.haze = area,
            BLUR => materials.blur = area,
            _ => debug_assert_eq!(material, NONE),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestClient;
    use super::super::{Rect, Server};
    use super::*;
    use crate::wayland::protocols::wl_compositor;
    use crate::wayland::wire::NewId;

    /// The client's registry is 2, its compositor 3 and its material
    /// global 4, with a surface 5 and its material object 6.
    fn surface(server: &mut Server) -> TestClient {
        let client = TestClient::new(server);
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(server, 2, &wl_compositor::INTERFACE, 3);
        client.bind(server, 2, &mikros_material_v1::INTERFACE, 4);
        client.send(server, 3, 0, vec![Arg::NewId(NewId(5))]);
        client.send(
            server,
            4,
            0,
            vec![Arg::NewId(NewId(6)), Arg::Object(Some(5))],
        );
        client
    }

    /// Makes the region `id` of `rect` and sets it as `material`.
    fn set(server: &mut Server, client: &TestClient, id: u32, material: u32, rect: [i32; 4]) {
        client.send(server, 3, 1, vec![Arg::NewId(NewId(id))]);
        client.send(server, id, 1, Vec::from(rect.map(Arg::Int)));
        client.send(
            server,
            6,
            0,
            vec![Arg::Uint(material), Arg::Object(Some(id))],
        );
    }

    fn materials(server: &Server) -> (&Materials, &Materials) {
        let (_, surface) = server.surfaces().next().unwrap();
        (&surface.pending.materials, &surface.current.materials)
    }

    #[test]
    fn applies_the_areas_of_materials_with_the_next_commit() {
        let mut server = Server::new();
        let client = surface(&mut server);
        set(&mut server, &client, 7, HAZE, [0, 0, 100, 40]);
        assert_eq!(client.error(&mut server), None);
        let (pending, current) = materials(&server);
        assert!(pending.haze.contains(99, 39));
        assert!(current.haze.is_empty());
        client.send(&mut server, 5, 6, vec![]);
        let (_, current) = materials(&server);
        assert!(current.haze.contains(99, 39));
        assert!(!current.haze.contains(100, 0));
    }

    #[test]
    fn cuts_each_area_out_of_the_other_material() {
        let mut server = Server::new();
        let client = surface(&mut server);
        set(&mut server, &client, 7, HAZE, [0, 0, 100, 40]);
        set(&mut server, &client, 8, BLUR, [0, 0, 50, 40]);
        set(&mut server, &client, 9, NONE, [90, 0, 10, 40]);
        let (pending, _) = materials(&server);
        assert!(pending.blur.contains(10, 10));
        assert!(!pending.haze.contains(10, 10));
        assert!(pending.haze.contains(60, 10));
        assert!(!pending.haze.contains(95, 10));
        let rect = Rect {
            x: 50,
            y: 0,
            width: 40,
            height: 40,
        };
        assert_eq!(pending.haze.rects(), [rect]);
    }

    #[test]
    fn refuses_unknown_materials() {
        let mut server = Server::new();
        let client = surface(&mut server);
        set(&mut server, &client, 7, 3, [0, 0, 10, 10]);
        let invalid = mikros_surface_material_v1::error::INVALID_MATERIAL;
        assert_eq!(client.error(&mut server), Some(invalid));
    }

    #[test]
    fn gives_a_surface_one_material_object_at_a_time() {
        let mut server = Server::new();
        let client = surface(&mut server);
        let again = || vec![Arg::NewId(NewId(7)), Arg::Object(Some(5))];
        client.send(&mut server, 4, 0, again());
        let exists = mikros_material_v1::error::SURFACE_EXISTS;
        assert_eq!(client.error(&mut server), Some(exists));
        // Once the first is destroyed, the surface may get another.
        let mut server = Server::new();
        let client = surface(&mut server);
        client.send(&mut server, 6, 1, vec![]);
        client.send(&mut server, 4, 0, again());
        assert_eq!(client.error(&mut server), None);
    }

    #[test]
    fn refuses_requests_after_the_surface_but_destroy() {
        let mut server = Server::new();
        let client = surface(&mut server);
        client.send(&mut server, 5, 0, vec![]);
        set(&mut server, &client, 7, HAZE, [0, 0, 10, 10]);
        let no_surface = mikros_surface_material_v1::error::NO_SURFACE;
        assert_eq!(client.error(&mut server), Some(no_surface));
        let client = surface(&mut server);
        client.send(&mut server, 5, 0, vec![]);
        client.send(&mut server, 6, 1, vec![]);
        assert_eq!(client.error(&mut server), None);
    }
}
