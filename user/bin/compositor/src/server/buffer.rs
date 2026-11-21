//! `mikros_buffer_v1`, `mikros_surface_sync_v1`, `mikros_timeline_v1` and
//! `wl_buffer` (MIKSPEC-0076.R23): buffers of export tokens and memory
//! objects, kernel timelines, and the acquire and release points that each
//! commit of a buffer sets. Importing is done by `Imports`, which the
//! program implements with its render device and the kernel.

use super::{ProtocolError, Request, Resource, Server, SurfaceId};
use crate::wayland::protocols::{
    mikros_buffer_v1, mikros_surface_sync_v1, mikros_timeline_v1, wl_buffer, wl_surface,
};
use crate::wayland::wire::{Arg, DecodeError, Fd};

/// A buffer or a timeline, as `Imports` knows it.
pub type BufferId = u64;
pub type TimelineId = u64;

/// What a buffer's handle is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Memory {
    /// An export token, imported with `ImportBuffer`.
    Token,
    /// A memory object, imported with `ImportMemory`.
    Object,
}

/// The layout of a buffer's pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    /// The DRM format and modifier.
    pub format: u32,
    pub modifier: u64,
    pub stride: u32,
    pub offset: u32,
}

/// A point on a kernel timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub timeline: TimelineId,
    pub value: u64,
}

/// What importing buffers and timelines takes.
pub trait Imports {
    /// Imports the handle of `memory` as a buffer of `layout`, or returns
    /// None when it cannot be imported.
    fn buffer(&mut self, memory: Memory, handle: Fd, layout: Layout) -> Option<BufferId>;
    /// Takes `handle`, or returns None when it is not a kernel timeline.
    fn timeline(&mut self, handle: Fd) -> Option<TimelineId>;
    /// Forgets a buffer or a timeline that nothing uses any more.
    fn drop_buffer(&mut self, buffer: BufferId);
    fn drop_timeline(&mut self, timeline: TimelineId);
    /// Copies the pixels of `buffer` as the output's wallpaper, or returns
    /// false when they cannot be copied.
    fn set_wallpaper(&mut self, buffer: BufferId) -> bool;
}

impl Server {
    pub(super) fn buffer_request(
        &mut self,
        imports: &mut dyn Imports,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let decoded =
            |e: DecodeError| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match resource {
            Resource::BufferFactory => {
                let decoded =
                    mikros_buffer_v1::Request::decode(request.opcode, args).map_err(decoded)?;
                self.buffer_factory(imports, request, decoded)?;
            }
            Resource::Buffer(_) => {
                let wl_buffer::Request::Destroy =
                    wl_buffer::Request::decode(request.opcode, args).map_err(decoded)?;
                self.client_mut(request).destroy(request.object);
                self.collect(imports);
            }
            Resource::Timeline(_) => {
                let mikros_timeline_v1::Request::Destroy =
                    mikros_timeline_v1::Request::decode(request.opcode, args).map_err(decoded)?;
                self.client_mut(request).destroy(request.object);
                self.collect(imports);
            }
            Resource::SurfaceSync(surface) => {
                let decoded = mikros_surface_sync_v1::Request::decode(request.opcode, args)
                    .map_err(decoded)?;
                self.surface_sync(request, surface, decoded)?;
            }
            _ => unreachable!("buffer_request handles buffers"),
        }
        Ok(())
    }

    fn buffer_factory(
        &mut self,
        imports: &mut dyn Imports,
        request: Request,
        decoded: mikros_buffer_v1::Request,
    ) -> Result<(), ProtocolError> {
        let error = |code, message: &str| ProtocolError::new(request.object, code, message);
        let (id, memory, handle, layout) = match decoded {
            mikros_buffer_v1::Request::CreateFromToken {
                id,
                token,
                width,
                height,
                format,
                modifier_hi,
                modifier_lo,
                stride,
                offset,
            } => {
                let modifier = u64::from(modifier_hi) << 32 | u64::from(modifier_lo);
                let layout = Layout {
                    width,
                    height,
                    format,
                    modifier,
                    stride,
                    offset,
                };
                (id, Memory::Token, token, layout)
            }
            mikros_buffer_v1::Request::CreateFromMemory {
                id,
                memory,
                width,
                height,
                format,
                stride,
                offset,
            } => {
                // Memory objects hold linear pixels.
                let layout = Layout {
                    width,
                    height,
                    format,
                    modifier: 0,
                    stride,
                    offset,
                };
                (id, Memory::Object, memory, layout)
            }
            mikros_buffer_v1::Request::ImportTimeline { id, timeline } => {
                let Some(timeline) = imports.timeline(timeline) else {
                    return Err(error(
                        mikros_buffer_v1::error::INVALID_TIMELINE,
                        "not a timeline",
                    ));
                };
                self.imported_timelines.insert(timeline);
                let c = self.client_mut(request);
                return c.create(
                    id,
                    &mikros_timeline_v1::INTERFACE,
                    request.version,
                    Resource::Timeline(timeline),
                );
            }
            mikros_buffer_v1::Request::GetSurfaceSync { id, surface } => {
                let c = self
                    .clients
                    .get_mut(&request.client)
                    .expect("a dispatched client");
                let Resource::Surface(surface) =
                    c.resource(request.object, surface, &wl_surface::INTERFACE)?
                else {
                    unreachable!("surfaces are wl_surface objects");
                };
                let s = self.surface_mut(surface)?;
                if s.sync.is_some() {
                    return Err(error(
                        mikros_buffer_v1::error::SURFACE_EXISTS,
                        "a sync object already",
                    ));
                }
                s.sync = Some(id.0);
                let c = self
                    .clients
                    .get_mut(&request.client)
                    .expect("a dispatched client");
                return c.create(
                    id,
                    &mikros_surface_sync_v1::INTERFACE,
                    request.version,
                    Resource::SurfaceSync(surface),
                );
            }
        };
        if layout.width == 0 || layout.height == 0 {
            return Err(error(
                mikros_buffer_v1::error::INVALID_BUFFER,
                "an empty buffer",
            ));
        }
        let Some(buffer) = imports.buffer(memory, handle, layout) else {
            return Err(error(
                mikros_buffer_v1::error::INVALID_BUFFER,
                "the buffer cannot be imported",
            ));
        };
        self.imported_buffers.insert(buffer);
        self.client_mut(request)
            .create(id, &wl_buffer::INTERFACE, 1, Resource::Buffer(buffer))
    }

    fn surface_sync(
        &mut self,
        request: Request,
        surface: SurfaceId,
        decoded: mikros_surface_sync_v1::Request,
    ) -> Result<(), ProtocolError> {
        let gone = || {
            ProtocolError::new(
                request.object,
                mikros_surface_sync_v1::error::NO_SURFACE,
                "the surface was destroyed",
            )
        };
        let (timeline, point_hi, point_lo, acquire) = match decoded {
            mikros_surface_sync_v1::Request::Destroy => {
                if let Some(s) = self
                    .surfaces
                    .get_mut(&surface)
                    .filter(|s| s.sync == Some(request.object))
                {
                    s.sync = None;
                    s.pending.acquire = None;
                    s.pending.release = None;
                }
                let c = self
                    .clients
                    .get_mut(&request.client)
                    .expect("a dispatched client");
                c.destroy(request.object);
                return Ok(());
            }
            mikros_surface_sync_v1::Request::SetAcquirePoint {
                timeline,
                point_hi,
                point_lo,
            } => (timeline, point_hi, point_lo, true),
            mikros_surface_sync_v1::Request::SetReleasePoint {
                timeline,
                point_hi,
                point_lo,
            } => (timeline, point_hi, point_lo, false),
        };
        let c = self
            .clients
            .get_mut(&request.client)
            .expect("a dispatched client");
        let Resource::Timeline(timeline) =
            c.resource(request.object, timeline, &mikros_timeline_v1::INTERFACE)?
        else {
            unreachable!("timelines are mikros_timeline_v1 objects");
        };
        let point = Point {
            timeline,
            value: u64::from(point_hi) << 32 | u64::from(point_lo),
        };
        let s = self
            .surfaces
            .get_mut(&surface)
            .filter(|s| s.sync == Some(request.object))
            .ok_or_else(gone)?;
        match acquire {
            true => s.pending.acquire = Some(point),
            false => s.pending.release = Some(point),
        }
        Ok(())
    }

    /// Checks the points of a commit of the surface `id` that has a sync
    /// object, which a buffer must have both of.
    pub(super) fn check_points(&mut self, id: SurfaceId) -> Result<(), ProtocolError> {
        let s = self.surface_mut(id)?;
        let Some(sync) = s.sync else {
            return Ok(());
        };
        if !s.attached || s.pending.buffer.is_none() {
            return Ok(());
        }
        let error = |code, message: &str| ProtocolError::new(sync, code, message);
        let (Some(acquire), Some(release)) = (s.pending.acquire, s.pending.release) else {
            return Err(error(
                mikros_surface_sync_v1::error::MISSING_POINT,
                "a buffer without both points",
            ));
        };
        if acquire.timeline == release.timeline && release.value <= acquire.value {
            return Err(error(
                mikros_surface_sync_v1::error::CONFLICTING_POINTS,
                "a release point not after the acquire point",
            ));
        }
        Ok(())
    }

    /// The release points of buffers that the surfaces no longer show,
    /// which the program reaches once no frame it draws reads them.
    pub fn take_releases(&mut self) -> Vec<Point> {
        std::mem::take(&mut self.releases)
    }

    /// Tells `imports` of the buffers and timelines whose objects are gone
    /// and that no surface uses.
    pub(super) fn collect(&mut self, imports: &mut dyn Imports) {
        let mut buffers = std::collections::BTreeSet::new();
        let mut timelines = std::collections::BTreeSet::new();
        for c in self.clients.values() {
            for object in c.objects.values() {
                match object.resource {
                    Resource::Buffer(b) => drop(buffers.insert(b)),
                    Resource::Timeline(t) => drop(timelines.insert(t)),
                    _ => {}
                }
            }
        }
        for s in self.surfaces.values() {
            for state in [&s.pending, &s.current] {
                buffers.extend(state.buffer);
                timelines.extend(
                    state
                        .acquire
                        .iter()
                        .chain(&state.release)
                        .map(|p| p.timeline),
                );
            }
        }
        timelines.extend(self.releases.iter().map(|p| p.timeline));
        self.imported_buffers.retain(|b| {
            let used = buffers.contains(b);
            if !used {
                imports.drop_buffer(*b);
            }
            used
        });
        self.imported_timelines.retain(|t| {
            let used = timelines.contains(t);
            if !used {
                imports.drop_timeline(*t);
            }
            used
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::surface::tests::with_surface;
    use super::super::tests::TestClient;
    use super::*;
    use crate::wayland::protocols::wl_display;
    use crate::wayland::wire::NewId;

    /// The compositor's buffer global of `server` bound by `client` as 5.
    fn bind(server: &mut Server, client: &TestClient) {
        client.bind(server, 2, &mikros_buffer_v1::INTERFACE, 5);
    }

    /// The request that makes the buffer `id` of the token `handle`.
    fn token(id: u32, handle: u32) -> Vec<Arg> {
        let mut args = vec![Arg::NewId(NewId(id)), Arg::Fd(Fd(handle))];
        args.extend([64, 32, 0x3432_5258, 0, 0, 256, 0].map(Arg::Uint));
        args
    }

    /// Sets the points `acquire` and `release` of the sync object 8 on
    /// the timeline 7.
    fn points(server: &mut Server, client: &TestClient, acquire: u32, release: u32) {
        client.send(
            server,
            8,
            0,
            vec![Arg::Object(Some(7)), Arg::Uint(0), Arg::Uint(acquire)],
        );
        client.send(
            server,
            8,
            1,
            vec![Arg::Object(Some(7)), Arg::Uint(0), Arg::Uint(release)],
        );
    }

    /// A client with a surface 4, the buffer global 5, the buffer 6, the
    /// timeline 7 and the surface's sync object 8.
    fn with_sync(server: &mut Server) -> TestClient {
        let client = with_surface(server);
        bind(server, &client);
        client.send(server, 5, 0, token(6, 40));
        client.send(server, 5, 2, vec![Arg::NewId(NewId(7)), Arg::Fd(Fd(41))]);
        client.send(
            server,
            5,
            3,
            vec![Arg::NewId(NewId(8)), Arg::Object(Some(4))],
        );
        client
    }

    fn attach(server: &mut Server, client: &TestClient, buffer: u32) {
        client.send(
            server,
            4,
            1,
            vec![Arg::Object(Some(buffer)), Arg::Int(0), Arg::Int(0)],
        );
        client.send(server, 4, 6, vec![]);
    }

    #[test]
    fn tells_clients_the_device_and_formats() {
        let mut server = Server::new();
        server.buffer_device = "basicgraphics".into();
        server.buffer_formats = vec![(0x3432_5258, 0)];
        let client = with_surface(&mut server);
        bind(&mut server, &client);
        let events = client.events(&mut server, &|_| &mikros_buffer_v1::EVENTS);
        assert_eq!(
            events[0],
            (5, 0, vec![Arg::String(Some("basicgraphics".into()))])
        );
        assert_eq!(
            events[1],
            (
                5,
                1,
                vec![Arg::Uint(0x3432_5258), Arg::Uint(0), Arg::Uint(0)]
            )
        );
        assert_eq!(events[2], (5, 2, vec![]));
    }

    #[test]
    fn shows_buffers_with_their_points_and_releases_the_last() {
        let mut server = Server::new();
        let client = with_sync(&mut server);
        points(&mut server, &client, 1, 2);
        attach(&mut server, &client, 6);
        assert_eq!(client.error(&mut server), None);
        let surface = server.surfaces().next().unwrap().1;
        let (buffer, timeline) = (1, 2);
        assert_eq!(surface.current.buffer, Some(buffer));
        assert_eq!(surface.current.release, Some(Point { timeline, value: 2 }));
        assert!(server.take_releases().is_empty());
        // A second buffer: the first one's release point is due, and the
        // first buffer, once its object is gone, is forgotten.
        client.send(&mut server, 5, 0, token(9, 42));
        points(&mut server, &client, 3, 4);
        attach(&mut server, &client, 9);
        assert_eq!(server.take_releases(), [Point { timeline, value: 2 }]);
        client.send(&mut server, 6, 0, vec![]);
        assert_eq!(client.imports.borrow().dropped_buffers, [buffer]);
        assert_eq!(client.error(&mut server), None);
    }

    #[test]
    fn refuses_buffers_and_timelines_it_cannot_import() {
        let mut server = Server::new();
        let client = with_surface(&mut server);
        bind(&mut server, &client);
        client.send(&mut server, 5, 0, token(6, 0));
        assert_eq!(
            client.error(&mut server),
            Some(mikros_buffer_v1::error::INVALID_BUFFER)
        );
        let client = with_surface(&mut server);
        bind(&mut server, &client);
        client.send(
            &mut server,
            5,
            2,
            vec![Arg::NewId(NewId(7)), Arg::Fd(Fd(0))],
        );
        assert_eq!(
            client.error(&mut server),
            Some(mikros_buffer_v1::error::INVALID_TIMELINE)
        );
        let client = with_sync(&mut server);
        client.send(
            &mut server,
            5,
            3,
            vec![Arg::NewId(NewId(9)), Arg::Object(Some(4))],
        );
        assert_eq!(
            client.error(&mut server),
            Some(mikros_buffer_v1::error::SURFACE_EXISTS)
        );
    }

    #[test]
    fn refuses_commits_without_good_points() {
        let mut server = Server::new();
        let client = with_sync(&mut server);
        attach(&mut server, &client, 6);
        assert_eq!(
            client.error(&mut server),
            Some(mikros_surface_sync_v1::error::MISSING_POINT)
        );
        let client = with_sync(&mut server);
        points(&mut server, &client, 5, 5);
        attach(&mut server, &client, 6);
        assert_eq!(
            client.error(&mut server),
            Some(mikros_surface_sync_v1::error::CONFLICTING_POINTS)
        );
        // A request of the sync object after its surface is gone.
        let client = with_sync(&mut server);
        client.send(&mut server, 4, 0, vec![]);
        points(&mut server, &client, 1, 2);
        assert_eq!(
            client.error(&mut server),
            Some(mikros_surface_sync_v1::error::NO_SURFACE)
        );
        // But destroying the sync object after its surface is not one.
        let client = with_sync(&mut server);
        client.send(&mut server, 4, 0, vec![]);
        client.send(&mut server, 8, 2, vec![]);
        assert_eq!(client.error(&mut server), None);
        let _ = wl_display::EVENTS;
    }
}
