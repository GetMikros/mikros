//! `wl_subcompositor` and `wl_subsurface` (MIKSPEC-0066.R16): a surface
//! with the role of a subsurface lies at a position within its parent,
//! in a stack with its parent and its siblings. A subsurface's position
//! and its place in the stack, which its requests change, take effect
//! when its parent's state is next applied. A subsurface starts on top,
//! in synchronized mode.
//!
//! A subsurface that was destroyed stops being a subsurface at once,
//! though its surface keeps the role; a surface whose parent was
//! destroyed keeps its `wl_subsurface` object, which does nothing more.

use super::{ClientId, ProtocolError, Request, Resource, Role, Server, SurfaceId};
use crate::wayland::protocols::{wl_subcompositor, wl_subsurface, wl_surface};
use crate::wayland::wire::{Arg, ObjectId};
use std::collections::BTreeMap;

pub const VERSION: u32 = 1;

/// A subsurface: its object, its parent, which is none once the parent
/// is gone, its mode and its position, as applied and as requested.
#[derive(Debug)]
struct Sub {
    client: ClientId,
    object: ObjectId,
    parent: Option<SurfaceId>,
    sync: bool,
    position: (i32, i32),
    pending_position: (i32, i32),
}

/// The stack of a parent and its subsurfaces, bottom first, as applied
/// and as requested since. The parent's own id is its place in it.
#[derive(Debug)]
struct Family {
    order: Vec<SurfaceId>,
    pending: Option<Vec<SurfaceId>>,
}

/// The subsurfaces of every surface.
#[derive(Debug, Default)]
pub(super) struct Subsurfaces {
    subs: BTreeMap<SurfaceId, Sub>,
    families: BTreeMap<SurfaceId, Family>,
}

impl Subsurfaces {
    /// Whether `surface` is `ancestor` or one of its descendants.
    fn is_within(&self, surface: SurfaceId, ancestor: SurfaceId) -> bool {
        let mut at = Some(surface);
        // A chain is no longer than the subsurfaces there are.
        for _ in 0..=self.subs.len() {
            match at {
                Some(s) if s == ancestor => return true,
                Some(s) => at = self.subs.get(&s).and_then(|sub| sub.parent),
                None => return false,
            }
        }
        false
    }

    /// The requested stack of `parent`.
    fn pending_stack(&mut self, parent: SurfaceId) -> &mut Vec<SurfaceId> {
        let family = self.families.entry(parent).or_insert_with(|| Family {
            order: vec![parent],
            pending: None,
        });
        family.pending.get_or_insert_with(|| family.order.clone())
    }

    /// Takes `surface` out of the stacks of its parent, and forgets it.
    fn remove(&mut self, surface: SurfaceId) {
        let Some(sub) = self.subs.remove(&surface) else {
            return;
        };
        if let Some(family) = sub.parent.and_then(|p| self.families.get_mut(&p)) {
            family.order.retain(|&s| s != surface);
            if let Some(pending) = &mut family.pending {
                pending.retain(|&s| s != surface);
            }
        }
    }

    /// Makes the requested stack and the requested positions of the
    /// subsurfaces of `parent` the applied ones.
    pub(super) fn apply_family(&mut self, parent: SurfaceId) {
        let Some(family) = self.families.get_mut(&parent) else {
            return;
        };
        if let Some(pending) = family.pending.take() {
            family.order = pending;
        }
        for child in &family.order {
            if let Some(sub) = self.subs.get_mut(child) {
                sub.position = sub.pending_position;
            }
        }
    }

    /// The applied stack of `parent`, bottom first, with its own id.
    #[cfg(test)]
    fn stack(&self, parent: SurfaceId) -> Vec<SurfaceId> {
        self.families
            .get(&parent)
            .map_or_else(|| vec![parent], |f| f.order.clone())
    }
}

impl Server {
    pub(super) fn subsurface_request(
        &mut self,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let invalid = |e| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match resource {
            Resource::Subcompositor => {
                match wl_subcompositor::Request::decode(request.opcode, args).map_err(invalid)? {
                    wl_subcompositor::Request::Destroy => {
                        self.client_mut(request).destroy(request.object);
                    }
                    wl_subcompositor::Request::GetSubsurface {
                        id,
                        surface,
                        parent,
                    } => self.get_subsurface(request, id, surface, parent)?,
                }
                Ok(())
            }
            Resource::Subsurface(surface) => {
                let decoded =
                    wl_subsurface::Request::decode(request.opcode, args).map_err(invalid)?;
                self.subsurface(request, surface, decoded)
            }
            _ => unreachable!("subsurface_request handles subsurfaces"),
        }
    }

    fn get_subsurface(
        &mut self,
        request: Request,
        id: crate::wayland::wire::NewId,
        surface: ObjectId,
        parent: ObjectId,
    ) -> Result<(), ProtocolError> {
        let c = self.client_mut(request);
        let interface = &wl_surface::INTERFACE;
        let (Resource::Surface(child), Resource::Surface(parent)) = (
            c.resource(request.object, surface, interface)?,
            c.resource(request.object, parent, interface)?,
        ) else {
            unreachable!("surfaces are wl_surface objects");
        };
        let error = |code, message: &str| ProtocolError::new(request.object, code, message);
        let s = self.surface_mut(child)?;
        if !matches!(s.role, Role::None | Role::Subsurface)
            || s.xdg.is_some()
            || self.subsurfaces.subs.contains_key(&child)
        {
            return Err(error(
                wl_subcompositor::error::BAD_SURFACE,
                "a surface that has another role or a subsurface",
            ));
        }
        if self.subsurfaces.is_within(parent, child) {
            return Err(error(
                wl_subcompositor::error::BAD_PARENT,
                "a parent that is the surface or its descendant",
            ));
        }
        let c = self.client_mut(request);
        let resource = Resource::Subsurface(child);
        c.create(id, &wl_subsurface::INTERFACE, request.version, resource)?;
        self.surface_mut(child)?.role = Role::Subsurface;
        self.subsurfaces.subs.insert(
            child,
            Sub {
                client: request.client,
                object: id.0,
                parent: Some(parent),
                sync: true,
                position: (0, 0),
                pending_position: (0, 0),
            },
        );
        self.subsurfaces.pending_stack(parent).push(child);
        Ok(())
    }

    fn subsurface(
        &mut self,
        request: Request,
        surface: SurfaceId,
        decoded: wl_subsurface::Request,
    ) -> Result<(), ProtocolError> {
        // The object of a surface that is no longer this one's subsurface
        // does nothing but go away.
        let live = self
            .subsurfaces
            .subs
            .get(&surface)
            .is_some_and(|s| s.object == request.object);
        if matches!(decoded, wl_subsurface::Request::Destroy) {
            self.client_mut(request).destroy(request.object);
            if live {
                self.subsurfaces.remove(surface);
            }
            return Ok(());
        }
        let parent = self.subsurfaces.subs.get(&surface).and_then(|s| s.parent);
        let (true, Some(parent)) = (live, parent) else {
            return Ok(());
        };
        match decoded {
            wl_subsurface::Request::Destroy => unreachable!("handled above"),
            wl_subsurface::Request::SetPosition { x, y } => {
                if let Some(sub) = self.subsurfaces.subs.get_mut(&surface) {
                    sub.pending_position = (x, y);
                }
            }
            wl_subsurface::Request::PlaceAbove { sibling } => {
                self.place_subsurface(request, surface, parent, sibling, 1)?;
            }
            wl_subsurface::Request::PlaceBelow { sibling } => {
                self.place_subsurface(request, surface, parent, sibling, 0)?;
            }
            wl_subsurface::Request::SetSync => self.set_sync(surface, true),
            wl_subsurface::Request::SetDesync => self.set_sync(surface, false),
        }
        Ok(())
    }

    /// Puts `surface` in the requested stack of `parent` next to
    /// `sibling`, above it for an `offset` of 1 and below for 0.
    fn place_subsurface(
        &mut self,
        request: Request,
        surface: SurfaceId,
        parent: SurfaceId,
        sibling: ObjectId,
        offset: usize,
    ) -> Result<(), ProtocolError> {
        let c = self.client_mut(request);
        let Resource::Surface(sibling) =
            c.resource(request.object, sibling, &wl_surface::INTERFACE)?
        else {
            unreachable!("surfaces are wl_surface objects");
        };
        let stack = self.subsurfaces.pending_stack(parent);
        if sibling == surface || !stack.contains(&sibling) {
            return Err(ProtocolError::new(
                request.object,
                wl_subsurface::error::BAD_SURFACE,
                "a sibling that is not the parent or another subsurface of it",
            ));
        }
        stack.retain(|&s| s != surface);
        let at = stack.iter().position(|&s| s == sibling).unwrap_or(0) + offset;
        stack.insert(at, surface);
        Ok(())
    }

    fn set_sync(&mut self, surface: SurfaceId, sync: bool) {
        if let Some(sub) = self.subsurfaces.subs.get_mut(&surface) {
            sub.sync = sync;
        }
    }

    /// Forgets the subsurface of `surface`, which was destroyed, and
    /// leaves its subsurfaces without a parent.
    pub(super) fn forget_surface_subsurfaces(&mut self, surface: SurfaceId) {
        self.subsurfaces.remove(surface);
        if let Some(family) = self.subsurfaces.families.remove(&surface) {
            for child in family.order {
                if let Some(sub) = self.subsurfaces.subs.get_mut(&child) {
                    sub.parent = None;
                }
            }
        }
    }

    /// Forgets the subsurfaces and stacks of `client`, which went.
    pub(super) fn forget_client_subsurfaces(&mut self, client: ClientId) {
        self.subsurfaces.subs.retain(|_, s| s.client != client);
        let subs = &self.subsurfaces.subs;
        self.subsurfaces
            .families
            .retain(|parent, _| subs.values().any(|s| s.parent == Some(*parent)));
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestClient;
    use super::*;
    use crate::wayland::protocols::wl_compositor;
    use crate::wayland::wire::NewId;

    /// A client with the compositor 3, the subcompositor 4, and the
    /// surfaces 5, 6, 7 and 8.
    fn connect(server: &mut Server) -> TestClient {
        let client = TestClient::new(server);
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(server, 2, &wl_compositor::INTERFACE, 3);
        client.bind(server, 2, &wl_subcompositor::INTERFACE, 4);
        for id in 5..=8 {
            client.send(server, 3, 0, vec![Arg::NewId(NewId(id))]);
        }
        client
    }

    /// The surface of the object `object` of the first client.
    fn surface(server: &Server, object: ObjectId) -> SurfaceId {
        *server
            .surfaces
            .iter()
            .find(|(_, s)| s.object == object)
            .unwrap()
            .0
    }

    /// Makes object `id` the subsurface of the surface `child` within `parent`.
    fn get(server: &mut Server, client: &TestClient, id: u32, child: u32, parent: u32) {
        let args = vec![
            Arg::NewId(NewId(id)),
            Arg::Object(Some(child)),
            Arg::Object(Some(parent)),
        ];
        client.send(server, 4, 1, args);
    }

    fn stack(server: &Server, parent: ObjectId) -> Vec<ObjectId> {
        let parent = surface(server, parent);
        let objects = |s: SurfaceId| server.surfaces[&s].object;
        server
            .subsurfaces
            .stack(parent)
            .into_iter()
            .map(objects)
            .collect()
    }

    fn commit(server: &mut Server, client: &TestClient, surface: u32) {
        client.send(server, surface, 6, vec![]);
    }

    #[test]
    fn a_new_subsurface_lies_above_its_parent_and_its_siblings() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 10, 6, 5);
        get(&mut server, &client, 11, 7, 5);
        assert_eq!(client.error(&mut server), None);
        // The stack and the positions are those of the parent's commit.
        assert_eq!(stack(&server, 5), [5]);
        commit(&mut server, &client, 5);
        assert_eq!(stack(&server, 5), [5, 6, 7]);
        let child = surface(&server, 6);
        assert!(server.subsurfaces.subs[&child].sync);
        assert_eq!(server.surfaces[&child].role, Role::Subsurface);
    }

    #[test]
    fn places_and_positions_take_effect_with_the_parent_commit() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 10, 6, 5);
        get(&mut server, &client, 11, 7, 5);
        commit(&mut server, &client, 5);
        client.send(&mut server, 10, 2, vec![Arg::Object(Some(7))]);
        client.send(&mut server, 11, 3, vec![Arg::Object(Some(5))]);
        client.send(&mut server, 10, 1, vec![Arg::Int(10), Arg::Int(20)]);
        assert_eq!(stack(&server, 5), [5, 6, 7]);
        let child = surface(&server, 6);
        assert_eq!(server.subsurfaces.subs[&child].position, (0, 0));
        // The child is above its sibling now and the sibling below the parent.
        commit(&mut server, &client, 5);
        assert_eq!(stack(&server, 5), [7, 5, 6]);
        assert_eq!(server.subsurfaces.subs[&child].position, (10, 20));
        assert_eq!(client.error(&mut server), None);
    }

    #[test]
    fn refuses_a_surface_that_has_a_role_or_a_subsurface() {
        for case in 0..3 {
            let mut server = Server::new();
            let client = connect(&mut server);
            match case {
                0 => {
                    let id = surface(&server, 6);
                    server.surfaces.get_mut(&id).unwrap().role = Role::Toplevel;
                }
                1 => {
                    let id = surface(&server, 6);
                    server.surfaces.get_mut(&id).unwrap().role = Role::Cursor;
                }
                _ => get(&mut server, &client, 10, 6, 5),
            }
            get(&mut server, &client, 11, 6, 7);
            let code = wl_subcompositor::error::BAD_SURFACE;
            assert_eq!(client.error(&mut server), Some(code), "case {case}");
        }
    }

    #[test]
    fn refuses_a_parent_that_is_the_surface_or_below_it() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 10, 5, 5);
        let code = wl_subcompositor::error::BAD_PARENT;
        assert_eq!(client.error(&mut server), Some(code));
        // 7 is below 6, which is below 5: 5 cannot lie within 7.
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 10, 6, 5);
        get(&mut server, &client, 11, 7, 6);
        assert_eq!(client.error(&mut server), None);
        get(&mut server, &client, 12, 5, 7);
        assert_eq!(client.error(&mut server), Some(code));
    }

    #[test]
    fn refuses_a_place_next_to_a_surface_that_is_not_a_sibling() {
        for sibling in [6, 8] {
            let mut server = Server::new();
            let client = connect(&mut server);
            get(&mut server, &client, 10, 6, 5);
            client.send(&mut server, 10, 2, vec![Arg::Object(Some(sibling))]);
            let code = wl_subsurface::error::BAD_SURFACE;
            assert_eq!(client.error(&mut server), Some(code), "sibling {sibling}");
        }
    }

    #[test]
    fn a_destroyed_subsurface_is_gone_at_once_but_its_surface_keeps_the_role() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 10, 6, 5);
        commit(&mut server, &client, 5);
        client.send(&mut server, 10, 0, vec![]);
        assert_eq!(stack(&server, 5), [5]);
        let child = surface(&server, 6);
        assert_eq!(server.surfaces[&child].role, Role::Subsurface);
        // It can be a subsurface again, of another parent.
        get(&mut server, &client, 11, 6, 7);
        assert_eq!(client.error(&mut server), None);
    }

    #[test]
    fn a_subsurface_whose_parent_was_destroyed_does_nothing() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 10, 6, 5);
        commit(&mut server, &client, 5);
        client.send(&mut server, 5, 0, vec![]);
        let child = surface(&server, 6);
        assert_eq!(server.subsurfaces.subs[&child].parent, None);
        client.send(&mut server, 10, 1, vec![Arg::Int(1), Arg::Int(1)]);
        client.send(&mut server, 10, 2, vec![Arg::Object(Some(7))]);
        client.send(&mut server, 10, 4, vec![]);
        assert_eq!(client.error(&mut server), None);
        client.send(&mut server, 10, 0, vec![]);
        assert!(server.subsurfaces.subs.is_empty());
    }

    #[test]
    fn a_disconnected_client_leaves_no_subsurfaces() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 10, 6, 5);
        get(&mut server, &client, 11, 7, 6);
        server.disconnect(&mut super::super::tests::FakeImports::default(), client.id);
        assert!(server.subsurfaces.subs.is_empty());
        assert!(server.subsurfaces.families.is_empty());
    }
}
