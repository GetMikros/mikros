//! The compositor's side of its clients' Wayland connections. Each
//! client has its objects, by ID, each an object of an interface at a
//! version that stands for a resource of the compositor. `Server::dispatch`
//! takes the messages of a channel message, decodes each request by its
//! object's interface, checks it against the object's version and hands it
//! to the code of the resource; a request that breaks the protocol is a
//! protocol error, which is sent to the client and ends its connection.

mod area;
mod buffer;
mod cursor_shape;
mod material;
mod panel;
mod scene;
mod seat;
mod shell;
mod surface;
mod theme;
mod xdg;

pub use buffer::{BufferId, Imports, Point, TimelineId};
#[cfg(target_os = "mikros")]
pub use buffer::{Layout, Memory};
pub use surface::{Rect, Role, Surface, SurfaceId};

use crate::wayland::protocols::{
    mikros_buffer_v1, mikros_material_v1, mikros_shell_v1, mikros_theme_v1, wl_callback,
    wl_compositor, wl_display, wl_output, wl_registry, wl_seat, wp_cursor_shape_manager_v1,
    xdg_wm_base,
};
use crate::wayland::wire::{self, Fd, Interface, NewId, ObjectId, Outgoing};
use std::collections::BTreeSet;
use std::collections::{BTreeMap, VecDeque};

/// The first ID of the objects that the compositor would create, beyond
/// those that clients create.
const SERVER_IDS: u32 = 0xff00_0000;

/// The ID of the display object of every client.
const DISPLAY: ObjectId = 1;

pub type ClientId = u64;

/// What a client's object stands for in the compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    Display,
    Registry,
    Callback,
    Compositor,
    Surface(SurfaceId),
    Region,
    WmBase,
    Positioner,
    XdgSurface(SurfaceId),
    Toplevel(SurfaceId),
    BufferFactory,
    Buffer(BufferId),
    Timeline(TimelineId),
    SurfaceSync(SurfaceId),
    Output,
    Shell,
    Materials,
    Material(SurfaceId),
    Theme,
    Panel(SurfaceId),
    Seat,
    Pointer,
    CursorShapes,
    CursorShape,
}

/// An object of a client.
#[derive(Debug)]
pub struct Object {
    pub interface: &'static Interface,
    pub version: u32,
    pub resource: Resource,
}

/// A protocol error, which ends the client's connection.
#[derive(Debug, PartialEq, Eq)]
pub struct ProtocolError {
    pub object: ObjectId,
    pub code: u32,
    pub message: String,
}

impl ProtocolError {
    pub fn new(object: ObjectId, code: u32, message: impl Into<String>) -> ProtocolError {
        ProtocolError {
            object,
            code,
            message: message.into(),
        }
    }

    fn invalid_object(message: impl Into<String>) -> ProtocolError {
        ProtocolError::new(DISPLAY, wl_display::error::INVALID_OBJECT, message)
    }

    fn invalid_method(object: ObjectId, message: impl Into<String>) -> ProtocolError {
        ProtocolError::new(object, wl_display::error::INVALID_METHOD, message)
    }
}

/// A client's connection.
#[derive(Default)]
pub struct Client {
    objects: BTreeMap<ObjectId, Object>,
    /// The roles of a connection of a shell component, which the
    /// compositor created; None for any other client.
    roles: Option<Vec<String>>,
    /// The cursor it chose for its surfaces.
    pub cursor: seat::Cursor,
    /// The events to send to the client.
    pub out: Outgoing,
    /// The protocol error that ended the connection, once one did.
    pub error: Option<ProtocolError>,
}

impl Client {
    fn new() -> Client {
        let mut client = Client::default();
        client.objects.insert(
            DISPLAY,
            Object {
                interface: &wl_display::INTERFACE,
                version: 1,
                resource: Resource::Display,
            },
        );
        client
    }

    /// Whether the client is a shell component of `role`.
    fn has_role(&self, role: &str) -> bool {
        self.roles
            .as_ref()
            .is_some_and(|r| r.iter().any(|x| x == role))
    }

    pub fn object(&self, id: ObjectId) -> Option<&Object> {
        self.objects.get(&id)
    }

    /// Adds the object `id` that a request of an object at `version`
    /// creates, as `resource` of `interface`.
    fn create(
        &mut self,
        id: NewId,
        interface: &'static Interface,
        version: u32,
        resource: Resource,
    ) -> Result<(), ProtocolError> {
        if id.0 >= SERVER_IDS || self.objects.contains_key(&id.0) {
            return Err(ProtocolError::invalid_object(format!(
                "invalid new ID {} for {}",
                id.0, interface.name
            )));
        }
        let object = Object {
            interface,
            version,
            resource,
        };
        self.objects.insert(id.0, object);
        Ok(())
    }

    /// Removes the object `id`, whose ID the client may then use again.
    fn destroy(&mut self, id: ObjectId) {
        if self.objects.remove(&id).is_some() {
            wl_display::send_delete_id(&mut self.out, DISPLAY, id);
        }
    }

    /// The resource of the object `id` that the request of `from` names,
    /// which must be of `interface`.
    fn resource(
        &self,
        from: ObjectId,
        id: ObjectId,
        interface: &Interface,
    ) -> Result<Resource, ProtocolError> {
        match self.objects.get(&id) {
            Some(object) if std::ptr::eq(object.interface, interface) => Ok(object.resource),
            _ => Err(ProtocolError::invalid_object(format!(
                "object {id} of a request of {from} is not a {}",
                interface.name
            ))),
        }
    }
}

/// A global that clients bind through their registries.
pub struct Global {
    pub name: u32,
    pub interface: &'static Interface,
    pub version: u32,
    /// What its objects stand for.
    pub resource: Resource,
    /// Whether only shell components see it.
    pub components_only: bool,
}

/// Every client and the state of the protocols they speak.
#[derive(Default)]
pub struct Server {
    clients: BTreeMap<ClientId, Client>,
    next_client: ClientId,
    pub globals: Vec<Global>,
    surfaces: BTreeMap<SurfaceId, Surface>,
    next_surface: SurfaceId,
    /// The rectangles of each client's regions, added or, when false,
    /// taken away, in order.
    regions: BTreeMap<(ClientId, ObjectId), Vec<(bool, Rect)>>,
    /// The serial of the last event that needs one.
    serial: u32,
    /// The size of the screen, which fullscreen toplevels get.
    pub screen: Option<(i32, i32)>,
    /// The name of the render device that clients allocate buffers on,
    /// and the formats and modifiers that the compositor imports.
    pub buffer_device: String,
    pub buffer_formats: Vec<(u32, u64)>,
    /// The buffers and timelines that `Imports` imported and has not yet
    /// been told to forget.
    imported_buffers: BTreeSet<BufferId>,
    /// The width and height of each imported buffer.
    buffer_sizes: BTreeMap<BufferId, (u32, u32)>,
    /// The seat's pointer.
    pub pointer: seat::Pointer,
    imported_timelines: BTreeSet<TimelineId>,
    /// The release points of buffers that surfaces no longer show.
    releases: Vec<Point>,
    /// The desktop's theme (MIKSPEC-0066.R19).
    theme: crate::theme::Theme,
}

impl Server {
    /// A server with the globals that every client gets.
    pub fn new() -> Server {
        let mut server = Server::default();
        server.add_global(
            &wl_compositor::INTERFACE,
            wl_compositor::VERSION,
            Resource::Compositor,
        );
        server.add_global(&xdg_wm_base::INTERFACE, 1, Resource::WmBase);
        server.add_global(&mikros_buffer_v1::INTERFACE, 1, Resource::BufferFactory);
        server.add_global(&wl_output::INTERFACE, 3, Resource::Output);
        server.add_global(&mikros_shell_v1::INTERFACE, 1, Resource::Shell);
        server.add_global(&mikros_material_v1::INTERFACE, 1, Resource::Materials);
        server.add_global(&mikros_theme_v1::INTERFACE, 1, Resource::Theme);
        server.add_global(&wl_seat::INTERFACE, seat::SEAT_VERSION, Resource::Seat);
        server.add_global(
            &wp_cursor_shape_manager_v1::INTERFACE,
            cursor_shape::VERSION,
            Resource::CursorShapes,
        );
        server
    }

    fn client_mut(&mut self, request: Request) -> &mut Client {
        self.clients
            .get_mut(&request.client)
            .expect("a dispatched client")
    }

    pub fn add_global(&mut self, interface: &'static Interface, version: u32, resource: Resource) {
        let name = self.globals.len() as u32 + 1;
        self.globals.push(Global {
            name,
            interface,
            version,
            resource,
            components_only: resource == Resource::Shell,
        });
    }

    /// Adds a client, and returns its ID.
    pub fn connect(&mut self) -> ClientId {
        self.next_client += 1;
        self.clients.insert(self.next_client, Client::new());
        self.next_client
    }

    /// Adds the connection of a shell component of `roles`, which the
    /// compositor created, and returns its ID.
    pub fn connect_component(&mut self, roles: Vec<String>) -> ClientId {
        let id = self.connect();
        if let Some(c) = self.clients.get_mut(&id) {
            c.roles = Some(roles);
        }
        id
    }

    /// Removes a client whose connection ended, with its surfaces.
    pub fn disconnect(&mut self, imports: &mut dyn Imports, client: ClientId) {
        self.clients.remove(&client);
        let releases = &mut self.releases;
        self.surfaces.retain(|_, s| {
            if s.client == client {
                releases.extend(s.current.release);
            }
            s.client != client
        });
        self.regions.retain(|(c, _), _| *c != client);
        self.collect(imports);
    }

    pub fn client(&mut self, client: ClientId) -> Option<&mut Client> {
        self.clients.get_mut(&client)
    }

    pub fn surfaces(&self) -> impl Iterator<Item = (&SurfaceId, &Surface)> {
        self.surfaces.iter()
    }

    fn next_serial(&mut self) -> u32 {
        self.serial = self.serial.wrapping_add(1);
        self.serial
    }

    /// Handles the requests in `bytes`, a channel message of `client`, with
    /// the handles of their file descriptors. A protocol error is sent to
    /// the client and recorded in its `error`, and ends its requests.
    pub fn dispatch(
        &mut self,
        imports: &mut dyn Imports,
        client: ClientId,
        bytes: &[u8],
        handles: Vec<Fd>,
    ) {
        let mut fds = VecDeque::from(handles);
        let mut rest = bytes;
        while !rest.is_empty() {
            if self.clients.get(&client).is_none_or(|c| c.error.is_some()) {
                return;
            }
            match self.message(imports, client, rest, &mut fds) {
                Ok(after) => rest = after,
                Err(error) => {
                    if let Some(c) = self.clients.get_mut(&client) {
                        let message = error.message.as_str();
                        wl_display::send_error(
                            &mut c.out,
                            DISPLAY,
                            error.object,
                            error.code,
                            message,
                        );
                        c.error = Some(error);
                    }
                    return;
                }
            }
        }
    }

    /// Handles the message at the start of `bytes`, and returns the bytes
    /// after it.
    fn message<'a>(
        &mut self,
        imports: &mut dyn Imports,
        client: ClientId,
        bytes: &'a [u8],
        fds: &mut VecDeque<Fd>,
    ) -> Result<&'a [u8], ProtocolError> {
        let (header, body, rest) = wire::split(bytes).map_err(|e| {
            ProtocolError::invalid_method(DISPLAY, format!("a broken message: {e:?}"))
        })?;
        let c = self.clients.get(&client).expect("a dispatched client");
        let Some(object) = c.objects.get(&header.sender) else {
            return Err(ProtocolError::invalid_object(format!(
                "invalid object {}",
                header.sender
            )));
        };
        let (interface, version, resource) = (object.interface, object.version, object.resource);
        let name = interface.name;
        let message = interface
            .requests
            .get(usize::from(header.opcode))
            .filter(|m| m.since <= version)
            .ok_or_else(|| {
                ProtocolError::invalid_method(
                    header.sender,
                    format!("invalid opcode {} for {name}", header.opcode),
                )
            })?;
        let args = wire::decode(message, body, fds).map_err(|e| {
            ProtocolError::invalid_method(
                header.sender,
                format!("invalid arguments for {name}.{}: {e:?}", message.name),
            )
        })?;
        let request = Request {
            client,
            object: header.sender,
            version,
            opcode: header.opcode,
        };
        let decoded = |e| ProtocolError::invalid_method(header.sender, format!("{e:?}"));
        match resource {
            Resource::Display => self.display(
                request,
                wl_display::Request::decode(header.opcode, args).map_err(decoded)?,
            )?,
            Resource::Registry => self.registry(
                request,
                wl_registry::Request::decode(header.opcode, args).map_err(decoded)?,
            )?,
            Resource::Callback => {}
            Resource::Compositor | Resource::Surface(_) | Resource::Region => {
                self.surface_request(imports, request, resource, args)?
            }
            Resource::BufferFactory
            | Resource::Buffer(_)
            | Resource::Timeline(_)
            | Resource::SurfaceSync(_) => self.buffer_request(imports, request, resource, args)?,
            Resource::WmBase
            | Resource::Positioner
            | Resource::XdgSurface(_)
            | Resource::Toplevel(_) => self.xdg_request(request, resource, args)?,
            Resource::Output => self.output_request(request, args)?,
            Resource::Shell => self.shell_request(imports, request, args)?,
            Resource::Materials | Resource::Material(_) => {
                self.material_request(request, resource, args)?
            }
            Resource::Panel(id) => self.panel_request(request, id, args)?,
            Resource::Seat | Resource::Pointer => self.seat_request(request, resource, args)?,
            Resource::CursorShapes | Resource::CursorShape => {
                self.cursor_shape_request(request, resource, args)?
            }
            Resource::Theme => {
                return Err(ProtocolError::invalid_method(
                    header.sender,
                    "the theme has no requests",
                ));
            }
        }
        Ok(rest)
    }

    fn display(
        &mut self,
        request: Request,
        decoded: wl_display::Request,
    ) -> Result<(), ProtocolError> {
        let serial = self.next_serial();
        let c = self
            .clients
            .get_mut(&request.client)
            .expect("a dispatched client");
        match decoded {
            wl_display::Request::Sync { callback } => {
                c.create(callback, &wl_callback::INTERFACE, 1, Resource::Callback)?;
                wl_callback::send_done(&mut c.out, callback.0, serial);
                c.destroy(callback.0);
            }
            wl_display::Request::GetRegistry { registry } => {
                c.create(registry, &wl_registry::INTERFACE, 1, Resource::Registry)?;
                let component = c.roles.is_some();
                for global in self
                    .globals
                    .iter()
                    .filter(|g| component || !g.components_only)
                {
                    let name = global.interface.name;
                    wl_registry::send_global(
                        &mut c.out,
                        registry.0,
                        global.name,
                        name,
                        global.version,
                    );
                }
            }
        }
        Ok(())
    }

    fn registry(
        &mut self,
        request: Request,
        decoded: wl_registry::Request,
    ) -> Result<(), ProtocolError> {
        let wl_registry::Request::Bind {
            name,
            id_interface,
            id_version,
            id,
        } = decoded;
        let component = self
            .clients
            .get(&request.client)
            .is_some_and(|c| c.roles.is_some());
        let global = self
            .globals
            .iter()
            .filter(|g| component || !g.components_only)
            .find(|g| g.name == name && g.interface.name == id_interface)
            .ok_or_else(|| {
                ProtocolError::invalid_object(format!("invalid global {id_interface} ({name})"))
            })?;
        if id_version == 0 || id_version > global.version {
            return Err(ProtocolError::invalid_object(format!(
                "invalid version {id_version} of {id_interface}"
            )));
        }
        let (interface, resource) = (global.interface, global.resource);
        let c = self
            .clients
            .get_mut(&request.client)
            .expect("a dispatched client");
        c.create(id, interface, id_version, resource)?;
        if resource == Resource::BufferFactory {
            mikros_buffer_v1::send_device(&mut c.out, id.0, &self.buffer_device);
            for &(format, modifier) in &self.buffer_formats {
                let (hi, lo) = ((modifier >> 32) as u32, modifier as u32);
                mikros_buffer_v1::send_format(&mut c.out, id.0, format, hi, lo);
            }
            mikros_buffer_v1::send_done(&mut c.out, id.0);
        }
        if resource == Resource::Output {
            self.describe_output(request.client, id.0, id_version);
        }
        if resource == Resource::Seat {
            self.describe_seat(request.client, id.0, id_version);
        }
        if resource == Resource::Theme {
            self.send_theme(request.client, id.0);
        }
        Ok(())
    }
}

/// Where a request came from.
#[derive(Clone, Copy, Debug)]
struct Request {
    client: ClientId,
    /// The object it was sent to.
    object: ObjectId,
    /// The object's version, which the objects it creates get.
    version: u32,
    opcode: u16,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::buffer::{Layout, Memory};
    use super::*;
    use crate::wayland::protocols::wl_callback;
    use crate::wayland::wire::{Arg, Message};

    /// A client of a server, which sends requests and reads the events
    /// the server sent it.
    pub struct TestClient {
        pub id: ClientId,
        pub imports: std::cell::RefCell<FakeImports>,
    }

    /// Imports that refuse the handle 0, number what they import, and
    /// record what they are told to forget.
    #[derive(Debug, Default)]
    pub struct FakeImports {
        next: u64,
        pub dropped_buffers: Vec<BufferId>,
        pub dropped_timelines: Vec<TimelineId>,
        pub wallpapers: Vec<BufferId>,
    }

    impl Imports for FakeImports {
        fn buffer(&mut self, _: Memory, handle: Fd, _: Layout) -> Option<BufferId> {
            self.next += 1;
            (handle.0 != 0).then_some(self.next)
        }

        fn timeline(&mut self, handle: Fd) -> Option<TimelineId> {
            self.next += 1;
            (handle.0 != 0).then_some(self.next)
        }

        fn drop_buffer(&mut self, buffer: BufferId) {
            self.dropped_buffers.push(buffer);
        }

        fn drop_timeline(&mut self, timeline: TimelineId) {
            self.dropped_timelines.push(timeline);
        }

        fn set_wallpaper(&mut self, buffer: BufferId) -> bool {
            self.wallpapers.push(buffer);
            true
        }
    }

    impl TestClient {
        pub fn new(server: &mut Server) -> TestClient {
            TestClient {
                id: server.connect(),
                imports: Default::default(),
            }
        }

        /// Sends the request `opcode` of `object` with `args`.
        pub fn send(&self, server: &mut Server, object: ObjectId, opcode: u16, args: Vec<Arg>) {
            let mut out = Outgoing::default();
            out.send(object, opcode, args);
            let (bytes, fds) = out.take().unwrap();
            server.dispatch(&mut *self.imports.borrow_mut(), self.id, &bytes, fds);
        }

        /// The events sent since the last call, as their object, opcode
        /// and arguments, decoded by the tables of `events`, which gives
        /// an object's events by its ID.
        pub fn events(
            &self,
            server: &mut Server,
            events: &dyn Fn(ObjectId) -> &'static [Message],
        ) -> Vec<(ObjectId, u16, Vec<Arg>)> {
            let c = server.client(self.id).unwrap();
            let mut all = Vec::new();
            while let Some((bytes, fds)) = c.out.take() {
                let mut fds = VecDeque::from(fds);
                let mut rest = bytes.as_slice();
                while !rest.is_empty() {
                    let (header, body, after) = wire::split(rest).unwrap();
                    let message = &events(header.sender)[usize::from(header.opcode)];
                    all.push((
                        header.sender,
                        header.opcode,
                        wire::decode(message, body, &mut fds).unwrap(),
                    ));
                    rest = after;
                }
            }
            all
        }

        /// Drops the events sent since the last call.
        pub fn drain(&self, server: &mut Server) {
            let c = server.client(self.id).unwrap();
            while c.out.take().is_some() {}
        }

        pub fn error(&self, server: &mut Server) -> Option<u32> {
            server
                .client(self.id)
                .unwrap()
                .error
                .as_ref()
                .map(|e| e.code)
        }

        /// Binds the global of `interface` as `id`.
        pub fn bind(
            &self,
            server: &mut Server,
            registry: ObjectId,
            interface: &Interface,
            id: u32,
        ) {
            let global = server
                .globals
                .iter()
                .find(|g| g.interface.name == interface.name)
                .unwrap();
            let args = vec![
                Arg::Uint(global.name),
                Arg::Untyped {
                    interface: interface.name.into(),
                    version: global.version,
                    id: NewId(id),
                },
            ];
            self.send(server, registry, 0, args);
        }
    }

    #[test]
    fn answers_sync_with_a_callback_that_it_destroys() {
        let mut server = Server::new();
        let client = TestClient::new(&mut server);
        client.send(&mut server, 1, 0, vec![Arg::NewId(NewId(2))]);
        let events = client.events(&mut server, &|id| match id {
            1 => &wl_display::EVENTS,
            _ => &wl_callback::EVENTS,
        });
        assert_eq!(events.len(), 2);
        assert_eq!((events[0].0, events[0].1), (2, 0));
        assert_eq!(events[1], (1, 1, vec![Arg::Uint(2)]));
        assert!(server.client(client.id).unwrap().object(2).is_none());
    }

    #[test]
    fn lists_and_binds_the_globals() {
        let mut server = Server::new();
        let client = TestClient::new(&mut server);
        client.send(&mut server, 1, 1, vec![Arg::NewId(NewId(2))]);
        let events = client.events(&mut server, &|_| &wl_registry::EVENTS);
        let names: Vec<&Arg> = events.iter().map(|e| &e.2[1]).collect();
        assert_eq!(
            names,
            [
                &Arg::String(Some("wl_compositor".into())),
                &Arg::String(Some("xdg_wm_base".into())),
                &Arg::String(Some("mikros_buffer_v1".into())),
                &Arg::String(Some("wl_output".into())),
                &Arg::String(Some("mikros_material_v1".into())),
                &Arg::String(Some("mikros_theme_v1".into())),
                &Arg::String(Some("wl_seat".into())),
                &Arg::String(Some("wp_cursor_shape_manager_v1".into()))
            ]
        );
        client.bind(&mut server, 2, &wl_compositor::INTERFACE, 3);
        assert_eq!(client.error(&mut server), None);
        let object = server.client(client.id).unwrap().object(3).unwrap();
        assert_eq!((object.version, object.resource), (6, Resource::Compositor));
    }

    #[test]
    fn ends_connections_that_break_the_protocol() {
        let mut server = Server::new();
        // An object that does not exist.
        let client = TestClient::new(&mut server);
        client.send(&mut server, 9, 0, vec![]);
        assert_eq!(
            client.error(&mut server),
            Some(wl_display::error::INVALID_OBJECT)
        );
        let events = client.events(&mut server, &|_| &wl_display::EVENTS);
        assert_eq!(events[0].0, 1);
        assert_eq!(events[0].2[0], Arg::Object(Some(1)));
        // An opcode that the interface lacks.
        let client = TestClient::new(&mut server);
        client.send(&mut server, 1, 7, vec![]);
        assert_eq!(
            client.error(&mut server),
            Some(wl_display::error::INVALID_METHOD)
        );
        // A new ID in use, a global that does not exist, and a version
        // beyond the global's.
        let client = TestClient::new(&mut server);
        client.send(&mut server, 1, 1, vec![Arg::NewId(NewId(1))]);
        assert_eq!(
            client.error(&mut server),
            Some(wl_display::error::INVALID_OBJECT)
        );
        let client = TestClient::new(&mut server);
        client.send(&mut server, 1, 1, vec![Arg::NewId(NewId(2))]);
        let bind = |name, version| {
            vec![
                Arg::Uint(name),
                Arg::Untyped {
                    interface: "wl_compositor".into(),
                    version,
                    id: NewId(3),
                },
            ]
        };
        client.send(&mut server, 2, 0, bind(9, 1));
        assert_eq!(
            client.error(&mut server),
            Some(wl_display::error::INVALID_OBJECT)
        );
        let client = TestClient::new(&mut server);
        client.send(&mut server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.send(&mut server, 2, 0, bind(1, 7));
        assert_eq!(
            client.error(&mut server),
            Some(wl_display::error::INVALID_OBJECT)
        );
    }
}
