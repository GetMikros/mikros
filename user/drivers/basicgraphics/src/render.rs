//! The render instance: `Render` connections whose buffers are anonymous
//! memory objects that the driver creates and hands out whatever the
//! flags, shared between connections through tokens, with sync objects
//! that are only registered. The clients draw into the memory
//! themselves, so the instance runs no GPU work: driver commands are
//! refused, and priorities are checked but change nothing.

use alloc::collections::BTreeMap;
use alloc::rc::{Rc, Weak};
use core::cell::{Cell, RefCell};
use mikros_gpudev::{Backend, Client, Screen};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::gpu::{
    Capabilities, DeviceInfo, DisplayAsyncServer, MemoryInfo, Render, RenderAsyncServer,
    RenderCreateBufferResponse, RenderExportBufferResponse, RenderGetMemoryInfoResponse,
    RenderImportBufferResponse, RenderImportSyncResponse, RenderPriority, RenderRequest,
    RenderSetPriorityResponse,
};
use mikros_rt::{midl, objects};

const PAGE_SIZE: u64 = 4096;
const SAME_RIGHTS: u32 = 1 << 31;
/// The permission that `HIGH` priority takes.
const GPU_PRIORITY: &str = "system.gpu_priority.use";
/// The share of the system's memory that the buffers created on one
/// connection may take, as a divisor, until drivers have a
/// configuration to set it in.
const CONNECTION_SHARE: u64 = 2;

/// A buffer's memory object, held by each connection that has the
/// buffer.
struct Memory {
    handle: OwnedHandle,
    size: u64,
    /// The event whose object ID is the buffer's token, once exported.
    token: RefCell<Option<OwnedHandle>>,
    /// The bytes of every buffer of the instance, which this one leaves
    /// once no connection has it.
    held: Rc<Cell<u64>>,
}

impl Drop for Memory {
    fn drop(&mut self) {
        self.held.set(self.held.get() - self.size);
    }
}

/// The render device, shared by its connections.
pub struct RenderDevice {
    reactor: Reactor,
    spawner: Spawner,
    /// The exported buffers by their token's object ID.
    exported: RefCell<BTreeMap<u64, Weak<Memory>>>,
    held: Rc<Cell<u64>>,
    system: u64,
}

/// A buffer of a connection, and whether it was created on it.
struct Held {
    memory: Rc<Memory>,
    created: bool,
}

/// What one `Render` connection has.
#[derive(Default)]
struct Connection {
    buffers: BTreeMap<u32, Held>,
    syncs: BTreeMap<u32, OwnedHandle>,
    /// The bytes of the buffers created on the connection that it still
    /// has.
    created: u64,
    next_id: u32,
}

impl Connection {
    /// An ID that no buffer or sync object of the connection has, never 0.
    fn id(&mut self) -> u32 {
        loop {
            self.next_id = self.next_id.wrapping_add(1).max(1);
            let id = self.next_id;
            if !self.buffers.contains_key(&id) && !self.syncs.contains_key(&id) {
                return id;
            }
        }
    }

    fn insert(&mut self, memory: Rc<Memory>, created: bool) -> u32 {
        let id = self.id();
        if created {
            self.created += memory.size;
        }
        self.buffers.insert(id, Held { memory, created });
        id
    }

    fn destroy(&mut self, buffer: u32) {
        if let Some(held) = self.buffers.remove(&buffer)
            && held.created
        {
            self.created -= held.memory.size;
        }
    }
}

impl RenderDevice {
    pub fn new(reactor: Reactor, spawner: Spawner) -> RenderDevice {
        RenderDevice {
            reactor,
            spawner,
            exported: RefCell::new(BTreeMap::new()),
            held: Rc::new(Cell::new(0)),
            system: objects::system_memory(),
        }
    }

    fn limit(&self) -> u64 {
        self.system / CONNECTION_SHARE
    }

    fn create(&self, connection: &mut Connection, size: u64) -> Result<(u32, OwnedHandle), Status> {
        if size == 0 {
            return Err(Status::InvalidArgs);
        }
        let size = size
            .checked_next_multiple_of(PAGE_SIZE)
            .ok_or(Status::NoResources)?;
        if connection.created.saturating_add(size) > self.limit() {
            return Err(Status::NoResources);
        }
        let handle = objects::memory(size, 0).map_err(|_| Status::NoMemory)?;
        let client =
            objects::duplicate(handle.raw(), SAME_RIGHTS).map_err(|_| Status::NoResources)?;
        self.held.set(self.held.get() + size);
        let memory = Rc::new(Memory {
            handle,
            size,
            token: RefCell::new(None),
            held: self.held.clone(),
        });
        Ok((connection.insert(memory, true), client))
    }

    /// The token of `buffer`, the same event each time it is exported.
    fn export(&self, connection: &Connection, buffer: u32) -> Result<OwnedHandle, Status> {
        let held = connection.buffers.get(&buffer).ok_or(Status::NotFound)?;
        let mut token = held.memory.token.borrow_mut();
        if token.is_none() {
            let event = objects::event(0).map_err(|_| Status::NoResources)?;
            let koid = midl::koid(event.raw()).ok_or(Status::NoResources)?;
            let mut exported = self.exported.borrow_mut();
            exported.retain(|_, memory| memory.strong_count() > 0);
            exported.insert(koid, Rc::downgrade(&held.memory));
            *token = Some(event);
        }
        let event = token.as_ref().ok_or(Status::NoResources)?;
        objects::duplicate(event.raw(), SAME_RIGHTS).map_err(|_| Status::NoResources)
    }

    /// The buffer that `token` names, with its size and memory object.
    fn import(
        &self,
        connection: &mut Connection,
        token: &OwnedHandle,
    ) -> Result<(u32, u64, OwnedHandle), Status> {
        let koid = midl::koid(token.raw()).ok_or(Status::NotFound)?;
        let memory = self
            .exported
            .borrow()
            .get(&koid)
            .and_then(Weak::upgrade)
            .ok_or(Status::NotFound)?;
        let client = objects::duplicate(memory.handle.raw(), SAME_RIGHTS)
            .map_err(|_| Status::NoResources)?;
        let size = memory.size;
        Ok((connection.insert(memory, false), size, client))
    }

    fn memory_info(&self, connection: &Connection) -> MemoryInfo {
        MemoryInfo {
            device_local_total: 0,
            device_local_used: 0,
            system_total: self.system,
            system_used: self.held.get(),
            connection_used: connection.created,
            connection_limit: self.limit(),
        }
    }

    /// Serves one `Render` connection until it closes, when its buffers
    /// and sync objects go.
    async fn serve(self: Rc<Self>, server: RenderAsyncServer, client: Client) {
        let mut connection = Connection::default();
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                RenderRequest::CreateBuffer(q, r) => {
                    let created = self.create(&mut connection, q.size);
                    r.reply(
                        s,
                        created.map(|(buffer, memory)| RenderCreateBufferResponse {
                            buffer,
                            memory: Some(memory),
                        }),
                    )
                }
                RenderRequest::DestroyBuffer(q) => {
                    connection.destroy(q.buffer);
                    Ok(())
                }
                RenderRequest::ExportBuffer(q, r) => {
                    let token = self.export(&connection, q.buffer);
                    r.reply(s, token.map(|token| RenderExportBufferResponse { token }))
                }
                RenderRequest::ImportBuffer(q, r) => {
                    let imported = self.import(&mut connection, &q.token);
                    r.reply(
                        s,
                        imported.map(|(buffer, size, memory)| RenderImportBufferResponse {
                            buffer,
                            size,
                            memory: Some(memory),
                        }),
                    )
                }
                RenderRequest::ImportSync(q, r) => {
                    let sync = connection.id();
                    connection.syncs.insert(sync, q.timeline);
                    r.reply(s, Ok(RenderImportSyncResponse { sync }))
                }
                RenderRequest::DestroySync(q) => {
                    connection.syncs.remove(&q.sync);
                    Ok(())
                }
                RenderRequest::DriverCommand(_, r) => r.reply(s, Err(Status::NotSupported)),
                RenderRequest::SetPriority(q, r) => {
                    let allowed =
                        q.priority != RenderPriority::High || client.holds(GPU_PRIORITY).await;
                    let result = match allowed {
                        true => Ok(RenderSetPriorityResponse {}),
                        false => Err(Status::AccessDenied),
                    };
                    r.reply(s, result)
                }
                RenderRequest::GetMemoryInfo(_, r) => r.reply(
                    s,
                    RenderGetMemoryInfoResponse {
                        info: self.memory_info(&connection),
                    },
                ),
                RenderRequest::ImportMemory(_, r) => r.reply(s, Err(Status::NotSupported)),
            };
        }
    }
}

impl Backend for RenderDevice {
    fn info(&self) -> DeviceInfo {
        crate::info(Capabilities::RENDER)
    }

    /// Never called: the library refuses `OpenDisplay` without `DISPLAY`.
    async fn serve(self: Rc<Self>, _display: Rc<DisplayAsyncServer>) {}

    fn closed(&self) {}

    fn open_render(
        self: Rc<Self>,
        render: ServerEnd<Render>,
        client: Client,
    ) -> Result<(), Status> {
        let server = RenderAsyncServer::new(render, self.reactor.clone());
        self.spawner.spawn(self.clone().serve(server, client));
        Ok(())
    }

    fn with_screen(&self, _draw: &mut dyn FnMut(&dyn Screen)) {}
}
