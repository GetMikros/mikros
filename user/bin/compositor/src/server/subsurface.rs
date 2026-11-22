//! `wl_subcompositor` and `wl_subsurface` (MIKSPEC-0066.R16): a surface
//! with the role of a subsurface lies at a position within its parent,
//! in a stack with its parent and its siblings. A subsurface's position
//! and its place in the stack, which its requests change, take effect
//! when its parent's state is next applied. A subsurface starts on top,
//! in synchronized mode, in which what its commits bring is held until its
//! parent's state is applied; a subsurface whose ancestor is synchronized
//! is held too. The held state of later commits merges into the earlier:
//! the last buffer wins and the earlier one is given up, and damage and
//! frame callbacks add up.
//!
//! A subsurface that was destroyed stops being a subsurface at once,
//! though its surface keeps the role; a surface whose parent was
//! destroyed keeps its `wl_subsurface` object, which does nothing more.

use super::surface::Staged;
use super::{ClientId, Imports, ProtocolError, Request, Resource, Role, Server, SurfaceId};
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
    /// What its commits brought since its parent's state was applied,
    /// while its mode and its ancestors' hold them.
    staged: Option<Staged>,
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
    fn remove(&mut self, surface: SurfaceId) -> Option<Sub> {
        let sub = self.subs.remove(&surface)?;
        if let Some(family) = sub.parent.and_then(|p| self.families.get_mut(&p)) {
            family.order.retain(|&s| s != surface);
            if let Some(pending) = &mut family.pending {
                pending.retain(|&s| s != surface);
            }
        }
        Some(sub)
    }

    /// Whether the commits of `surface` are held: it is a subsurface
    /// with a parent, and it or an ancestor is synchronized. A
    /// subsurface without a parent is not held.
    fn is_synchronized(&self, surface: SurfaceId) -> bool {
        let mut at = surface;
        for _ in 0..=self.subs.len() {
            let Some(sub) = self.subs.get(&at) else {
                return false;
            };
            let Some(parent) = sub.parent else {
                return false;
            };
            if sub.sync {
                return true;
            }
            at = parent;
        }
        false
    }

    /// The subsurfaces of `parent` in its applied stack, bottom first.
    fn children(&self, parent: SurfaceId) -> Vec<SurfaceId> {
        self.families.get(&parent).map_or_else(Vec::new, |f| {
            f.order.iter().copied().filter(|&s| s != parent).collect()
        })
    }

    /// Every subsurface of `parent`, whether or not its stack has been
    /// applied since it became one.
    fn all_children(&self, parent: SurfaceId) -> Vec<SurfaceId> {
        let of_parent = |sub: &&Sub| sub.parent == Some(parent);
        let children = self.subs.iter().filter(|(_, sub)| of_parent(sub));
        children.map(|(&child, _)| child).collect()
    }

    /// What the subsurfaces hold, for the buffers and points in use.
    pub(super) fn held(&self) -> impl Iterator<Item = &Staged> {
        self.subs.values().filter_map(|s| s.staged.as_ref())
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
        imports: &mut dyn Imports,
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
                self.subsurface(imports, request, surface, decoded)
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
                staged: None,
            },
        );
        self.subsurfaces.pending_stack(parent).push(child);
        Ok(())
    }

    fn subsurface(
        &mut self,
        imports: &mut dyn Imports,
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
            if live && let Some(sub) = self.subsurfaces.remove(surface) {
                self.give_up_held(surface, sub);
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
            wl_subsurface::Request::SetDesync => {
                self.set_sync(surface, false);
                self.flush_held(imports, request.client, surface);
            }
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
    /// leaves its subsurfaces without a parent, which applies what they
    /// held.
    pub(super) fn forget_surface_subsurfaces(
        &mut self,
        imports: &mut dyn Imports,
        surface: SurfaceId,
    ) {
        if let Some(sub) = self.subsurfaces.remove(surface) {
            self.give_up_held(surface, sub);
        }
        self.subsurfaces.families.remove(&surface);
        let children: Vec<SurfaceId> = self
            .subsurfaces
            .subs
            .iter()
            .filter(|(_, sub)| sub.parent == Some(surface))
            .map(|(&child, _)| child)
            .collect();
        for child in children {
            let Some(sub) = self.subsurfaces.subs.get_mut(&child) else {
                continue;
            };
            sub.parent = None;
            let client = sub.client;
            self.flush_held(imports, client, child);
        }
    }

    /// What a subsurface that is gone held: the release points of its
    /// buffer, its frame callbacks, which the next frame answers, and its
    /// presentation feedback, which is discarded.
    fn give_up_held(&mut self, surface: SurfaceId, sub: Sub) {
        let Some(held) = sub.staged else {
            return;
        };
        self.releases.extend(held.state.release);
        if let Some(s) = self.surfaces.get_mut(&surface) {
            s.frames.extend(held.frames);
        }
        self.discard_listed(held.asked);
    }

    /// Takes what `staged` brings, from a commit of `surface`, that its
    /// mode and its ancestors hold, into what it held, and returns what
    /// to apply now: nothing while it is held, otherwise the commit and
    /// what was held, merged.
    pub(super) fn hold_or_merge(&mut self, surface: SurfaceId, staged: Staged) -> Option<Staged> {
        let held = self
            .subsurfaces
            .subs
            .get_mut(&surface)
            .map(|sub| sub.staged.take());
        let Some(held) = held else {
            return Some(staged);
        };
        let merged = match held {
            Some(held) => self.merge_staged(held, staged),
            None => staged,
        };
        if self.subsurfaces.is_synchronized(surface) {
            if let Some(sub) = self.subsurfaces.subs.get_mut(&surface) {
                sub.staged = Some(merged);
            }
            return None;
        }
        Some(merged)
    }

    /// `later`, a commit after `earlier`, on top of `earlier`: the state
    /// that stays pending and the last buffer are the later's, a buffer
    /// that the later replaces is given up with its release point and its
    /// presentation feedback, and damage and frame callbacks add up.
    fn merge_staged(&mut self, earlier: Staged, later: Staged) -> Staged {
        let Staged {
            mut state,
            attached,
            mut frames,
            mut asked,
        } = later;
        let mut damage = earlier.state.damage;
        damage.extend(std::mem::take(&mut state.damage));
        let mut buffer_damage = earlier.state.buffer_damage;
        buffer_damage.extend(std::mem::take(&mut state.buffer_damage));
        state.damage = damage;
        state.buffer_damage = buffer_damage;
        let mut earlier_frames = earlier.frames;
        earlier_frames.append(&mut frames);
        let mut earlier_asked = earlier.asked;
        let mut attached_now = attached;
        if attached {
            if earlier.attached {
                self.releases.extend(earlier.state.release);
                self.discard_listed(std::mem::take(&mut earlier_asked));
            }
        } else {
            // The buffer is the earlier commit's.
            state.buffer = earlier.state.buffer;
            state.acquire = earlier.state.acquire;
            state.release = earlier.state.release;
            attached_now = earlier.attached;
        }
        earlier_asked.append(&mut asked);
        state.limit_damage();
        Staged {
            state,
            attached: attached_now,
            frames: earlier_frames,
            asked: earlier_asked,
        }
    }

    /// Applies what the subsurfaces below `parent`, which `parent`'s state
    /// that was just applied releases, held, and installs their stacks and
    /// positions, parents first.
    pub(super) fn cascade(&mut self, client: ClientId, parent: SurfaceId) {
        let mut work = vec![parent];
        while let Some(at) = work.pop() {
            self.subsurfaces.apply_family(at);
            let children = self.subsurfaces.children(at);
            for &child in &children {
                let held = self
                    .subsurfaces
                    .subs
                    .get_mut(&child)
                    .and_then(|s| s.staged.take());
                if let Some(held) = held {
                    self.apply_state(client, child, held);
                }
            }
            work.extend(children.into_iter().rev());
        }
    }

    /// Applies what the subsurfaces below `surface`, and it, hold that
    /// they no longer need to, after a change of mode or of parent.
    fn flush_held(&mut self, imports: &mut dyn Imports, client: ClientId, surface: SurfaceId) {
        let mut work = vec![surface];
        while let Some(at) = work.pop() {
            let free = !self.subsurfaces.is_synchronized(at);
            let held = self
                .subsurfaces
                .subs
                .get_mut(&at)
                .filter(|_| free)
                .and_then(|s| s.staged.take());
            if let Some(held) = held {
                self.apply_state(client, at, held);
                self.cascade(client, at);
            }
            work.extend(self.subsurfaces.all_children(at));
        }
        self.collect(imports);
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
    use super::super::Point;
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
    /// Attaches the buffer `buffer`, of 10 by 10 pixels, to the surface of
    /// `object` as a request would, with the release point `release`.
    fn attach(server: &mut Server, object: ObjectId, buffer: u64, release: Option<u64>) {
        let id = surface(server, object);
        server.buffer_sizes.insert(buffer, (10, 10));
        server.imported_buffers.insert(buffer);
        let s = server.surfaces.get_mut(&id).unwrap();
        s.pending.buffer = Some(buffer);
        s.pending.release = release.map(|value| Point { timeline: 1, value });
        s.attached = true;
    }

    fn shown_buffer(server: &Server, object: ObjectId) -> Option<u64> {
        server.surfaces[&surface(server, object)].current.buffer
    }

    #[test]
    fn a_synchronized_commit_waits_for_the_parents() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 10, 6, 5);
        attach(&mut server, 6, 1, None);
        // A frame callback of the child, which the child's commit brings.
        client.send(&mut server, 6, 3, vec![Arg::NewId(NewId(20))]);
        commit(&mut server, &client, 6);
        server.frames_done(5);
        assert_eq!(shown_buffer(&server, 6), None);
        let child = surface(&server, 6);
        assert!(server.surfaces[&child].frames.is_empty());
        assert!(server.subsurfaces.subs[&child].staged.is_some());
        commit(&mut server, &client, 5);
        assert_eq!(shown_buffer(&server, 6), Some(1));
        assert!(server.subsurfaces.subs[&child].staged.is_none());
        assert_eq!(server.surfaces[&child].frames, [20]);
    }

    #[test]
    fn held_commits_merge_with_the_last_buffer_and_the_damage_of_all() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 10, 6, 5);
        attach(&mut server, 6, 1, Some(5));
        client.send(&mut server, 6, 2, Vec::from([0, 0, 2, 2].map(Arg::Int)));
        commit(&mut server, &client, 6);
        attach(&mut server, 6, 2, Some(7));
        client.send(&mut server, 6, 2, Vec::from([4, 4, 2, 2].map(Arg::Int)));
        commit(&mut server, &client, 6);
        // The first buffer was never shown: its release point is due.
        assert_eq!(
            server.take_releases(),
            [Point {
                timeline: 1,
                value: 5
            }]
        );
        commit(&mut server, &client, 5);
        let child = surface(&server, 6);
        let current = &server.surfaces[&child].current;
        assert_eq!(current.buffer, Some(2));
        assert_eq!(
            current.release,
            Some(Point {
                timeline: 1,
                value: 7
            })
        );
        assert_eq!(current.damage.len(), 2);
        assert_eq!(client.error(&mut server), None);
    }

    #[test]
    fn a_desynchronized_commit_applies_at_once_and_so_does_what_was_held() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 10, 6, 5);
        attach(&mut server, 6, 1, None);
        commit(&mut server, &client, 6);
        assert_eq!(shown_buffer(&server, 6), None);
        // Desynchronizing applies what it held.
        client.send(&mut server, 10, 5, vec![]);
        assert_eq!(shown_buffer(&server, 6), Some(1));
        attach(&mut server, 6, 2, None);
        commit(&mut server, &client, 6);
        assert_eq!(shown_buffer(&server, 6), Some(2));
        // Synchronized again, it waits again.
        client.send(&mut server, 10, 4, vec![]);
        attach(&mut server, 6, 3, None);
        commit(&mut server, &client, 6);
        assert_eq!(shown_buffer(&server, 6), Some(2));
    }

    #[test]
    fn a_subsurface_below_a_synchronized_one_is_held_with_it() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 10, 6, 5);
        get(&mut server, &client, 11, 7, 6);
        // The grandchild is desynchronized, but its parent holds it.
        client.send(&mut server, 11, 5, vec![]);
        attach(&mut server, 7, 1, None);
        commit(&mut server, &client, 7);
        attach(&mut server, 6, 2, None);
        commit(&mut server, &client, 6);
        assert_eq!(
            (shown_buffer(&server, 6), shown_buffer(&server, 7)),
            (None, None)
        );
        commit(&mut server, &client, 5);
        assert_eq!(
            (shown_buffer(&server, 6), shown_buffer(&server, 7)),
            (Some(2), Some(1))
        );
    }

    #[test]
    fn what_a_subsurface_held_is_applied_when_its_parent_is_destroyed() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 10, 6, 5);
        attach(&mut server, 6, 1, None);
        commit(&mut server, &client, 6);
        client.send(&mut server, 5, 0, vec![]);
        assert_eq!(shown_buffer(&server, 6), Some(1));
        // Without a parent it applies at once.
        attach(&mut server, 6, 2, None);
        commit(&mut server, &client, 6);
        assert_eq!(shown_buffer(&server, 6), Some(2));
    }

    #[test]
    fn a_held_buffer_stays_imported_and_a_destroyed_subsurface_gives_its_release_up() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 10, 6, 5);
        attach(&mut server, 6, 1, Some(9));
        commit(&mut server, &client, 6);
        assert!(client.imports.borrow().dropped_buffers.is_empty());
        client.send(&mut server, 10, 0, vec![]);
        assert_eq!(
            server.take_releases(),
            [Point {
                timeline: 1,
                value: 9
            }]
        );
    }
}
