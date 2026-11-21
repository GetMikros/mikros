//! `wl_compositor`, `wl_surface` and `wl_region`: surfaces with the state
//! that requests set and a commit applies, the regions that set their
//! opaque and input areas, and the frame callbacks that the compositor
//! answers once it has drawn a frame.

use super::{BufferId, DISPLAY, Imports, Point, ProtocolError, Request, Resource, Server};
use crate::wayland::protocols::{
    wl_buffer, wl_callback, wl_compositor, wl_display, wl_region, wl_surface,
};
use crate::wayland::wire::{Arg, DecodeError, ObjectId};

pub type SurfaceId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// What a surface shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    None,
    Toplevel,
    Panel,
    /// The cursor of its client's pointer.
    Cursor,
}

/// The state of a surface that its requests set and a commit applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceState {
    /// The buffer, or None for no buffer.
    pub buffer: Option<BufferId>,
    /// The points on kernel timelines of the buffer, which its client
    /// reaches once the buffer holds what it drew, and which the
    /// compositor reaches once it no longer reads the buffer.
    pub acquire: Option<Point>,
    pub release: Option<Point>,
    /// The damage in surface coordinates and in buffer coordinates.
    pub damage: Vec<Rect>,
    pub buffer_damage: Vec<Rect>,
    /// The opaque and input regions, each rectangle added or, when false,
    /// taken away, in order. No input region is the whole surface.
    pub opaque: Vec<(bool, Rect)>,
    pub input: Option<Vec<(bool, Rect)>>,
    pub scale: i32,
    pub transform: u32,
    /// How far the buffer moves from the last one, which the next commit
    /// applies.
    pub offset: (i32, i32),
    /// The areas of its materials (MIKSPEC-0076.R16).
    pub materials: super::material::Materials,
}

impl Default for SurfaceState {
    fn default() -> SurfaceState {
        SurfaceState {
            buffer: None,
            acquire: None,
            release: None,
            damage: Vec::new(),
            buffer_damage: Vec::new(),
            opaque: Vec::new(),
            input: None,
            scale: 1,
            transform: wl_output_transform::NORMAL,
            offset: (0, 0),
            materials: Default::default(),
        }
    }
}

use crate::wayland::protocols::wl_output::transform as wl_output_transform;

/// A surface of a client.
#[derive(Debug)]
pub struct Surface {
    pub client: super::ClientId,
    pub object: ObjectId,
    pub role: Role,
    pub pending: SurfaceState,
    /// Whether a buffer was attached, or none, since the last commit.
    pub(super) attached: bool,
    /// The surface's mikros_surface_sync_v1 object, while it has one.
    pub sync: Option<ObjectId>,
    /// The committed state, whose buffer is the one that frames show.
    pub current: SurfaceState,
    /// The buffers that commits attached after it with an acquire point
    /// that no frame found reached yet, oldest first.
    pub(super) waiting: Vec<Waiting>,
    /// The frame callbacks of the pending state, and those of the
    /// committed state, which the next frame answers.
    pub(super) pending_frames: Vec<ObjectId>,
    pub frames: Vec<ObjectId>,
    /// The state of its xdg_surface, once it has one.
    pub xdg: Option<super::xdg::XdgState>,
    /// The state of its panel, while it has one.
    pub panel: Option<super::panel::PanelState>,
    /// Its mikros_surface_material_v1 object, while it has one.
    pub material: Option<ObjectId>,
}

/// A buffer that a commit attached with its acquire point, which frames
/// show once a frame finds that point reached, and until then the one
/// before it (MIKSPEC-0074.R14): its release point, and the damage of its
/// commit in surface coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Waiting {
    pub buffer: BufferId,
    pub acquire: Point,
    pub release: Option<Point>,
    pub damage: Vec<Rect>,
}

/// The most buffers that wait for their acquire points on a surface; a
/// commit past them gives up the oldest, whose damage the next keeps.
const WAITING_MAX: usize = 16;
/// The most `add` and `subtract` requests of a region, and the most
/// damage rectangles of a surface between commits, past which they
/// become the one rectangle that holds them (MIKSPEC-0074.R26).
const REGION_RECTS_MAX: usize = 1024;
const DAMAGE_MAX: usize = 256;

/// The smallest rectangle that holds `rects`.
fn bounds(rects: impl IntoIterator<Item = Rect>) -> Option<Rect> {
    rects
        .into_iter()
        .map(|r| {
            (
                r.x,
                r.y,
                r.x.saturating_add(r.width),
                r.y.saturating_add(r.height),
            )
        })
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
        .map(|(x0, y0, x1, y1)| Rect {
            x: x0,
            y: y0,
            width: x1.saturating_sub(x0),
            height: y1.saturating_sub(y0),
        })
}

impl SurfaceState {
    /// Makes the damage, past its most rectangles, the one rectangle that
    /// holds them all. Damage and buffer damage count together, as the
    /// compositor takes both in the same coordinates.
    fn limit_damage(&mut self) {
        if self.damage.len() + self.buffer_damage.len() <= DAMAGE_MAX {
            return;
        }
        let all: Vec<Rect> = self
            .damage
            .drain(..)
            .chain(self.buffer_damage.drain(..))
            .collect();
        self.damage.extend(bounds(all));
    }
}

impl Surface {
    /// The release points of the buffers that it shows and that wait.
    pub(super) fn releases(&self) -> impl Iterator<Item = Point> + '_ {
        let waiting = self.waiting.iter().filter_map(|w| w.release);
        self.current.release.into_iter().chain(waiting)
    }
}

impl Server {
    pub(super) fn surface_request(
        &mut self,
        imports: &mut dyn Imports,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let decoded =
            |e: DecodeError| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match resource {
            Resource::Compositor => self.compositor(
                request,
                wl_compositor::Request::decode(request.opcode, args).map_err(decoded)?,
            ),
            Resource::Surface(id) => self.surface(
                imports,
                request,
                id,
                wl_surface::Request::decode(request.opcode, args).map_err(decoded)?,
            ),
            Resource::Region => self.region(
                request,
                wl_region::Request::decode(request.opcode, args).map_err(decoded)?,
            ),
            _ => unreachable!("surface_request handles surfaces"),
        }
    }

    fn compositor(
        &mut self,
        request: Request,
        decoded: wl_compositor::Request,
    ) -> Result<(), ProtocolError> {
        let c = self
            .clients
            .get_mut(&request.client)
            .expect("a dispatched client");
        match decoded {
            wl_compositor::Request::CreateSurface { id } => {
                self.next_surface += 1;
                let surface = self.next_surface;
                c.create(
                    id,
                    &wl_surface::INTERFACE,
                    request.version,
                    Resource::Surface(surface),
                )?;
                self.surfaces.insert(
                    surface,
                    Surface {
                        client: request.client,
                        object: id.0,
                        role: Role::None,
                        pending: SurfaceState::default(),
                        attached: false,
                        sync: None,
                        current: SurfaceState::default(),
                        waiting: Vec::new(),
                        pending_frames: Vec::new(),
                        frames: Vec::new(),
                        xdg: None,
                        panel: None,
                        material: None,
                    },
                );
            }
            wl_compositor::Request::CreateRegion { id } => {
                c.create(id, &wl_region::INTERFACE, request.version, Resource::Region)?;
                self.regions.insert((request.client, id.0), Vec::new());
            }
        }
        Ok(())
    }

    fn region(
        &mut self,
        request: Request,
        decoded: wl_region::Request,
    ) -> Result<(), ProtocolError> {
        let key = (request.client, request.object);
        let (add, rect) = match decoded {
            wl_region::Request::Destroy => {
                self.regions.remove(&key);
                let c = self
                    .clients
                    .get_mut(&request.client)
                    .expect("a dispatched client");
                c.destroy(request.object);
                return Ok(());
            }
            wl_region::Request::Add {
                x,
                y,
                width,
                height,
            } => (
                true,
                Rect {
                    x,
                    y,
                    width,
                    height,
                },
            ),
            wl_region::Request::Subtract {
                x,
                y,
                width,
                height,
            } => (
                false,
                Rect {
                    x,
                    y,
                    width,
                    height,
                },
            ),
        };
        let rects = self.regions.entry(key).or_default();
        if rects.len() >= REGION_RECTS_MAX {
            return Err(ProtocolError::new(
                DISPLAY,
                wl_display::error::NO_MEMORY,
                format!("more than {REGION_RECTS_MAX} rectangles in a region"),
            ));
        }
        rects.push((add, rect));
        Ok(())
    }

    /// The rectangles of the region `region` of a request of `request`.
    fn region_of(
        &self,
        request: Request,
        region: Option<ObjectId>,
    ) -> Result<Option<Vec<(bool, Rect)>>, ProtocolError> {
        let Some(region) = region else {
            return Ok(None);
        };
        let c = self
            .clients
            .get(&request.client)
            .expect("a dispatched client");
        c.resource(request.object, region, &wl_region::INTERFACE)?;
        Ok(Some(
            self.regions
                .get(&(request.client, region))
                .cloned()
                .unwrap_or_default(),
        ))
    }

    fn surface(
        &mut self,
        imports: &mut dyn Imports,
        request: Request,
        id: SurfaceId,
        decoded: wl_surface::Request,
    ) -> Result<(), ProtocolError> {
        let error = |code, message: &str| ProtocolError::new(request.object, code, message);
        match decoded {
            wl_surface::Request::Destroy => {
                if let Some(surface) = self.surfaces.remove(&id) {
                    self.releases.extend(surface.releases());
                }
                self.client_mut(request).destroy(request.object);
                self.collect(imports);
                return Ok(());
            }
            wl_surface::Request::Attach { buffer, x, y } => {
                if request.version >= 5 && (x, y) != (0, 0) {
                    return Err(error(
                        wl_surface::error::INVALID_OFFSET,
                        "attach with an offset",
                    ));
                }
                let buffer = match buffer {
                    None => None,
                    Some(object) => {
                        let c = self.client_mut(request);
                        match c.resource(request.object, object, &wl_buffer::INTERFACE)? {
                            Resource::Buffer(buffer) => Some(buffer),
                            _ => unreachable!("buffers are wl_buffer objects"),
                        }
                    }
                };
                let surface = self.surface_mut(id)?;
                surface.pending.buffer = buffer;
                surface.pending.offset = (x, y);
                surface.attached = true;
            }
            wl_surface::Request::Damage {
                x,
                y,
                width,
                height,
            } => {
                let pending = &mut self.surface_mut(id)?.pending;
                pending.damage.push(Rect {
                    x,
                    y,
                    width,
                    height,
                });
                pending.limit_damage();
            }
            wl_surface::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                let pending = &mut self.surface_mut(id)?.pending;
                pending.buffer_damage.push(Rect {
                    x,
                    y,
                    width,
                    height,
                });
                pending.limit_damage();
            }
            wl_surface::Request::Frame { callback } => {
                let c = self
                    .clients
                    .get_mut(&request.client)
                    .expect("a dispatched client");
                c.create(callback, &wl_callback::INTERFACE, 1, Resource::Callback)?;
                self.surface_mut(id)?.pending_frames.push(callback.0);
            }
            wl_surface::Request::SetOpaqueRegion { region } => {
                let rects = self.region_of(request, region)?.unwrap_or_default();
                self.surface_mut(id)?.pending.opaque = rects;
            }
            wl_surface::Request::SetInputRegion { region } => {
                let rects = self.region_of(request, region)?;
                self.surface_mut(id)?.pending.input = rects;
            }
            wl_surface::Request::SetBufferTransform { transform } => {
                let Ok(transform) = u32::try_from(transform) else {
                    return Err(error(
                        wl_surface::error::INVALID_TRANSFORM,
                        "an unknown transform",
                    ));
                };
                if transform > wl_output_transform::FLIPPED_270 {
                    return Err(error(
                        wl_surface::error::INVALID_TRANSFORM,
                        "an unknown transform",
                    ));
                }
                self.surface_mut(id)?.pending.transform = transform;
            }
            wl_surface::Request::SetBufferScale { scale } => {
                if scale < 1 {
                    return Err(error(wl_surface::error::INVALID_SCALE, "a scale below 1"));
                }
                self.surface_mut(id)?.pending.scale = scale;
            }
            wl_surface::Request::Offset { x, y } => {
                self.surface_mut(id)?.pending.offset = (x, y);
            }
            wl_surface::Request::Commit => self.commit(imports, request, id)?,
        }
        Ok(())
    }

    /// The surface `id`, which a request names after its client destroyed
    /// it only when the client broke the protocol.
    pub(super) fn surface_mut(&mut self, id: SurfaceId) -> Result<&mut Surface, ProtocolError> {
        self.surfaces
            .get_mut(&id)
            .ok_or_else(|| ProtocolError::invalid_object("a destroyed surface"))
    }

    /// Applies the pending state of the surface `id`, after its role
    /// checked it.
    fn commit(
        &mut self,
        imports: &mut dyn Imports,
        request: Request,
        id: SurfaceId,
    ) -> Result<(), ProtocolError> {
        self.check_commit(request, id)?;
        self.check_panel_commit(id)?;
        self.check_points(id)?;
        let surface = self.surfaces.get_mut(&id).expect("a checked surface");
        let pending = &mut surface.pending;
        let current = &mut surface.current;
        // A new buffer, or new areas of its materials, change what the
        // whole surface shows when its client damaged nothing.
        let changed = surface.attached || pending.materials != current.materials;
        // A buffer's points are those of the commit that attached it. A
        // buffer with an acquire point waits for a frame that finds it
        // reached; any other replaces what the surface shows at once, and
        // what waits: the compositor no longer reads those.
        let mut waits = false;
        if surface.attached {
            surface.attached = false;
            match (pending.buffer, pending.acquire.take()) {
                (Some(buffer), Some(acquire)) => {
                    if surface.waiting.len() >= WAITING_MAX {
                        let oldest = surface.waiting.remove(0);
                        self.releases.extend(oldest.release);
                        surface.waiting[0].damage.extend(oldest.damage);
                    }
                    surface.waiting.push(Waiting {
                        buffer,
                        acquire,
                        release: pending.release.take(),
                        damage: Vec::new(),
                    });
                    waits = true;
                }
                (buffer, _) => {
                    self.releases.extend(current.release.take());
                    let waited = surface.waiting.drain(..);
                    self.releases.extend(waited.filter_map(|w| w.release));
                    current.buffer = buffer;
                    current.acquire = None;
                    current.release = pending.release.take();
                }
            }
        }
        pending.acquire = None;
        pending.release = None;
        current.damage = std::mem::take(&mut pending.damage);
        current.buffer_damage = std::mem::take(&mut pending.buffer_damage);
        current.opaque = pending.opaque.clone();
        current.input = pending.input.clone();
        current.scale = pending.scale;
        current.transform = pending.transform;
        current.offset = std::mem::take(&mut pending.offset);
        current.materials = pending.materials.clone();
        let frames = std::mem::take(&mut surface.pending_frames);
        surface.frames.extend(frames);
        self.committed(request, id);
        self.panel_committed(id);
        self.commit_damage(id, changed, waits);
        self.collect(imports);
        Ok(())
    }

    /// Shows, on each surface, the newest of the buffers that wait whose
    /// acquire point `reached` says is reached, with the damage of its
    /// commit and those before it, and gives up the buffers that it
    /// replaces.
    pub fn settle(&mut self, reached: impl Fn(Point) -> bool) {
        for (&id, s) in self.surfaces.iter_mut() {
            let Some(newest) = s.waiting.iter().rposition(|w| reached(w.acquire)) else {
                continue;
            };
            let settled: Vec<Waiting> = s.waiting.drain(..=newest).collect();
            self.releases.extend(s.current.release.take());
            let damage = self.committed.entry(id).or_default();
            for w in &settled[..newest] {
                self.releases.extend(w.release);
                damage.extend(&w.damage);
            }
            let shown = &settled[newest];
            damage.extend(&shown.damage);
            s.current.buffer = Some(shown.buffer);
            s.current.acquire = Some(shown.acquire);
            s.current.release = shown.release;
        }
    }

    /// The acquire points that buffers wait for.
    pub fn awaited(&self) -> Vec<Point> {
        let waiting = self.surfaces.values().flat_map(|s| &s.waiting);
        waiting.map(|w| w.acquire).collect()
    }

    /// Answers the frame callbacks of every committed state, once the
    /// compositor has drawn a frame at `time`, in milliseconds.
    pub fn frames_done(&mut self, time: u32) {
        for surface in self.surfaces.values_mut() {
            let Some(c) = self.clients.get_mut(&surface.client) else {
                continue;
            };
            for callback in surface.frames.drain(..) {
                wl_callback::send_done(&mut c.out, callback, time);
                c.destroy(callback);
            }
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::tests::TestClient;
    use super::*;
    use crate::wayland::wire::NewId;

    const OBJECTS_LIMIT: u32 = super::super::OBJECTS_MAX as u32;

    /// A client with the compositor bound as object 3 and a surface as 4.
    pub(crate) fn with_surface(server: &mut Server) -> TestClient {
        let client = TestClient::new(server);
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(server, 2, &wl_compositor::INTERFACE, 3);
        client.send(server, 3, 0, vec![Arg::NewId(NewId(4))]);
        client.drain(server);
        client
    }

    fn surface(server: &Server) -> &Surface {
        server.surfaces().next().unwrap().1
    }

    fn rect_args(x: i32, y: i32, width: i32, height: i32) -> Vec<Arg> {
        Vec::from([x, y, width, height].map(Arg::Int))
    }

    #[test]
    fn refuses_objects_past_the_limit() {
        let mut server = Server::new();
        let client = with_surface(&mut server);
        // The display, the registry, the compositor and the surface, and
        // then regions up to the limit.
        for id in 5..=OBJECTS_LIMIT {
            client.send(&mut server, 3, 1, vec![Arg::NewId(NewId(id))]);
        }
        assert_eq!(client.error(&mut server), None);
        client.send(
            &mut server,
            3,
            1,
            vec![Arg::NewId(NewId(OBJECTS_LIMIT + 1))],
        );
        assert_eq!(
            client.error(&mut server),
            Some(wl_display::error::NO_MEMORY)
        );
    }

    #[test]
    fn refuses_regions_of_too_many_rectangles() {
        let mut server = Server::new();
        let client = with_surface(&mut server);
        client.send(&mut server, 3, 1, vec![Arg::NewId(NewId(5))]);
        for i in 0..REGION_RECTS_MAX as i32 {
            let opcode = if i % 2 == 0 { 1 } else { 2 };
            client.send(&mut server, 5, opcode, rect_args(i, 0, 1, 1));
        }
        assert_eq!(client.error(&mut server), None);
        client.send(&mut server, 5, 1, rect_args(0, 0, 1, 1));
        assert_eq!(
            client.error(&mut server),
            Some(wl_display::error::NO_MEMORY)
        );
    }

    #[test]
    fn merges_damage_past_the_limit() {
        let mut server = Server::new();
        let client = with_surface(&mut server);
        for i in 0..DAMAGE_MAX as i32 / 2 {
            client.send(&mut server, 4, 2, rect_args(i, 10, 1, 1));
            client.send(&mut server, 4, 9, rect_args(5, i, 1, 1));
        }
        assert_eq!(surface(&server).pending.damage.len(), DAMAGE_MAX / 2);
        client.send(&mut server, 4, 2, rect_args(300, 300, 10, 10));
        assert_eq!(client.error(&mut server), None);
        let pending = &surface(&server).pending;
        assert_eq!(
            pending.damage,
            [Rect {
                x: 0,
                y: 0,
                width: 310,
                height: 310
            }]
        );
        assert!(pending.buffer_damage.is_empty());
    }

    #[test]
    fn applies_pending_state_on_commit() {
        let mut server = Server::new();
        let client = with_surface(&mut server);
        let damage = vec![Arg::Int(1), Arg::Int(2), Arg::Int(30), Arg::Int(40)];
        client.send(&mut server, 4, 2, damage);
        client.send(&mut server, 4, 8, vec![Arg::Int(2)]);
        assert_eq!(surface(&server).current, SurfaceState::default());
        client.send(&mut server, 4, 6, vec![]);
        let current = &surface(&server).current;
        let rect = Rect {
            x: 1,
            y: 2,
            width: 30,
            height: 40,
        };
        assert_eq!((current.damage.clone(), current.scale), (vec![rect], 2));
        assert!(surface(&server).pending.damage.is_empty());
        assert_eq!(client.error(&mut server), None);
    }

    #[test]
    fn sets_regions_and_answers_frames_after_a_commit() {
        let mut server = Server::new();
        let client = with_surface(&mut server);
        client.send(&mut server, 3, 1, vec![Arg::NewId(NewId(5))]);
        client.send(
            &mut server,
            5,
            1,
            vec![Arg::Int(0), Arg::Int(0), Arg::Int(10), Arg::Int(10)],
        );
        client.send(
            &mut server,
            5,
            2,
            vec![Arg::Int(0), Arg::Int(0), Arg::Int(5), Arg::Int(5)],
        );
        client.send(&mut server, 4, 4, vec![Arg::Object(Some(5))]);
        client.send(&mut server, 5, 0, vec![]);
        client.send(&mut server, 4, 3, vec![Arg::NewId(NewId(5))]);
        client.drain(&mut server);
        // Not before a commit.
        server.frames_done(16);
        assert!(server.client(client.id).unwrap().out.is_empty());
        client.send(&mut server, 4, 6, vec![]);
        assert_eq!(surface(&server).current.opaque.len(), 2);
        assert!(!surface(&server).current.opaque[1].0);
        server.frames_done(16);
        let events = client.events(&mut server, &|id| match id {
            1 => &wl_display::EVENTS,
            _ => &wl_callback::EVENTS,
        });
        assert_eq!(events[0], (5, 0, vec![Arg::Uint(16)]));
        assert_eq!(events[1], (1, 1, vec![Arg::Uint(5)]));
        assert_eq!(client.error(&mut server), None);
    }

    #[test]
    fn refuses_bad_scales_offsets_and_buffers() {
        let mut server = Server::new();
        let client = with_surface(&mut server);
        client.send(&mut server, 4, 8, vec![Arg::Int(0)]);
        assert_eq!(
            client.error(&mut server),
            Some(wl_surface::error::INVALID_SCALE)
        );
        let client = with_surface(&mut server);
        client.send(
            &mut server,
            4,
            1,
            vec![Arg::Object(None), Arg::Int(1), Arg::Int(0)],
        );
        assert_eq!(
            client.error(&mut server),
            Some(wl_surface::error::INVALID_OFFSET)
        );
        let client = with_surface(&mut server);
        client.send(
            &mut server,
            4,
            1,
            vec![Arg::Object(Some(3)), Arg::Int(0), Arg::Int(0)],
        );
        assert_eq!(
            client.error(&mut server),
            Some(wl_display::error::INVALID_OBJECT)
        );
        server.disconnect(&mut *client.imports.borrow_mut(), client.id);
        assert_eq!(server.surfaces().count(), 2);
    }
}
