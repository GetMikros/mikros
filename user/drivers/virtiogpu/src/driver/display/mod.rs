//! The `Display` connection of the display owner: a connector, CRTC,
//! primary plane and cursor plane for each scanout, dumb buffers and
//! imported memory objects as 2D resources, and commits that set the
//! scanouts, copy the damaged parts to the host and flush them.

mod commit;

use crate::driver::gpu::{Gpu, Resource};
use crate::protocol::{self, FORMAT_B8G8R8A8, FORMAT_B8G8R8X8};
use alloc::collections::BTreeMap;
use alloc::rc::{Rc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_display::rects::Rect;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::hardware::Mapping;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::gpu::{
    Connector, ConnectorCaps, ConnectorType, DisplayAsyncServer, DisplayCommitResponse,
    DisplayCreateDumbBufferResponse, DisplayFlags, DisplayGetResourcesResponse,
    DisplayImportMemoryRequest, DisplayImportMemoryResponse, DisplayImportSyncResponse,
    DisplayRequest, DisplayResources, DumbBuffer, FormatModifier, Mode, Plane, PlaneType,
};
use mikros_rt::objects;

/// The fourcc codes of `XRGB8888` and `ARGB8888`, and the linear
/// modifier.
pub const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
pub const ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
const LINEAR: u64 = 0;
/// The size of a cursor image, and the largest buffer.
const CURSOR: u32 = 64;
const MAX_SIZE: u32 = 16384;
const PERM_READ: u32 = 1 << 0;
const PAGE_SIZE: u64 = 4096;
const SAME_RIGHTS: u32 = 1 << 31;

/// Where a buffer's pixels come from: its resource's own backing, or a
/// client's memory object, copied into the backing before it is shown.
enum Source {
    Dumb,
    Imported {
        mapping: Mapping,
        _memory: OwnedHandle,
        /// Where the first line starts in the mapping, and bytes per line.
        start: u64,
        pitch: u32,
    },
}

struct Buffer {
    resource: Rc<Resource>,
    source: Source,
    format: u32,
    destroyed: bool,
}

/// What a primary plane shows.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Shown {
    buffer: u32,
    src: Rect,
}

/// What a cursor plane shows: its buffer and position.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Cursor {
    buffer: u32,
    x: u32,
    y: u32,
}

/// A scanout, with its connector, CRTC and planes.
pub struct Output {
    pub index: u32,
    pub enabled: bool,
    /// The modes, the scanout's current size first.
    pub modes: Vec<Mode>,
    pub edid: Vec<u8>,
}

/// What one CRTC shows and whether a commit for it is in flight.
#[derive(Clone, Copy, Default)]
struct Crtc {
    active: bool,
    mode: Option<(u32, u32)>,
    shown: Option<Shown>,
    cursor: Option<Cursor>,
    pending: bool,
}

/// What a commit changes on one CRTC.
struct Change {
    index: usize,
    active: bool,
    mode: Option<(u32, u32)>,
    shown: Option<Shown>,
    cursor: Option<Cursor>,
    damage: Vec<Rect>,
    /// Whether the commit lists the CRTC or its primary plane, and
    /// otherwise leaves the scanout as it is, and only the cursor changes.
    scanout: bool,
}

pub struct Display {
    pub gpu: Rc<Gpu>,
    reactor: Reactor,
    spawner: Spawner,
    pub outputs: RefCell<Vec<Output>>,
    crtcs: RefCell<Vec<Crtc>>,
    buffers: RefCell<BTreeMap<u32, Buffer>>,
    syncs: RefCell<BTreeMap<u32, Rc<OwnedHandle>>>,
    next_id: Cell<u32>,
    next_commit: Cell<u64>,
    /// The buffers that commits in flight show, by commit.
    showing: RefCell<BTreeMap<u64, Vec<u32>>>,
    /// Counts closed connections, so that a commit of a closed one
    /// changes nothing once presented.
    epoch: Cell<u64>,
    /// The owner's connection, for `OnHotplug`.
    pub owner: RefCell<Weak<DisplayAsyncServer>>,
}

/// The plane IDs of output `i`: its primary plane and its cursor plane.
fn primary_plane(i: usize) -> u32 {
    2 * i as u32 + 1
}

fn cursor_plane(i: usize) -> u32 {
    2 * i as u32 + 2
}

impl Display {
    pub fn new(gpu: Rc<Gpu>, outputs: Vec<Output>, reactor: Reactor, spawner: Spawner) -> Display {
        let crtcs = vec![Crtc::default(); outputs.len()];
        Display {
            gpu,
            reactor,
            spawner,
            outputs: RefCell::new(outputs),
            crtcs: RefCell::new(crtcs),
            buffers: RefCell::new(BTreeMap::new()),
            syncs: RefCell::new(BTreeMap::new()),
            next_id: Cell::new(1),
            next_commit: Cell::new(1),
            showing: RefCell::new(BTreeMap::new()),
            epoch: Cell::new(0),
            owner: RefCell::new(Weak::new()),
        }
    }

    fn id(&self) -> u32 {
        let id = self.next_id.get();
        self.next_id.set(id.wrapping_add(1).max(1));
        id
    }

    fn resources(&self) -> DisplayResources {
        let outputs = self.outputs.borrow();
        let formats = |list: &[u32]| {
            list.iter()
                .map(|&format| FormatModifier {
                    format,
                    modifier: LINEAR,
                })
                .collect::<Vec<_>>()
        };
        let mut planes = Vec::new();
        for (i, _) in outputs.iter().enumerate() {
            planes.push(Plane {
                id: primary_plane(i),
                r#type: PlaneType::Primary,
                possible_crtcs: 1 << i,
                max_width: MAX_SIZE,
                max_height: MAX_SIZE,
                formats: formats(&[XRGB8888, ARGB8888]),
            });
            planes.push(Plane {
                id: cursor_plane(i),
                r#type: PlaneType::Cursor,
                possible_crtcs: 1 << i,
                max_width: CURSOR,
                max_height: CURSOR,
                formats: formats(&[ARGB8888]),
            });
        }
        DisplayResources {
            flags: DisplayFlags(0),
            connectors: outputs
                .iter()
                .enumerate()
                .map(|(i, o)| Connector {
                    id: i as u32 + 1,
                    r#type: ConnectorType::Virtual,
                    connected: o.enabled,
                    possible_crtcs: 1 << i,
                    caps: ConnectorCaps(0),
                    gamma_size: 0,
                    modes: o.modes.iter().map(copy_mode).collect(),
                    edid: o.edid.clone(),
                })
                .collect(),
            crtcs: (1..=outputs.len() as u32).collect(),
            planes,
        }
    }

    fn insert(&self, resource: Resource, source: Source, format: u32) -> u32 {
        let buffer = self.id();
        let entry = Buffer {
            resource: Rc::new(resource),
            source,
            format,
            destroyed: false,
        };
        self.buffers.borrow_mut().insert(buffer, entry);
        buffer
    }

    async fn create_dumb(
        &self,
        width: u32,
        height: u32,
        format: u32,
    ) -> Result<DumbBuffer, Status> {
        let device_format = device_format(format).ok_or(Status::InvalidArgs)?;
        if width == 0 || height == 0 {
            return Err(Status::InvalidArgs);
        }
        if width > MAX_SIZE || height > MAX_SIZE {
            return Err(Status::OutOfRange);
        }
        let resource = self.gpu.create(width, height, device_format).await?;
        let memory = objects::duplicate(resource.memory.raw(), SAME_RIGHTS);
        let (stride, size) = (resource.stride, resource.size);
        let buffer = self.insert(resource, Source::Dumb, format);
        let memory = memory.map_err(|_| Status::NoResources)?;
        Ok(DumbBuffer {
            buffer,
            stride,
            size,
            memory,
        })
    }

    /// Makes a buffer of the image of `q` in its memory object, which is
    /// copied into a resource of the driver's own whenever it is shown,
    /// once the object is one whose mapping cannot fault and the image
    /// fits in it.
    async fn import_memory(&self, q: DisplayImportMemoryRequest) -> Result<u32, Status> {
        let device_format = device_format(q.format).ok_or(Status::InvalidArgs)?;
        let line = q.width.checked_mul(4).ok_or(Status::InvalidArgs)?;
        if q.width == 0
            || q.height == 0
            || q.width > MAX_SIZE
            || q.height > MAX_SIZE
            || q.stride < line
            || q.stride % 4 != 0
            || q.offset % 4 != 0
        {
            return Err(Status::InvalidArgs);
        }
        let len = u64::from(q.stride) * u64::from(q.height);
        let end = q.offset.checked_add(len).ok_or(Status::InvalidArgs)?;
        let size = objects::mappable_size(q.memory.raw()).ok_or(Status::InvalidArgs)?;
        if end > size {
            return Err(Status::InvalidArgs);
        }
        let first = q.offset & !(PAGE_SIZE - 1);
        let process = self.gpu.process();
        let mapping = Mapping::new(process, PERM_READ, q.memory.raw(), first, end - first)
            .map_err(|_| Status::InvalidArgs)?;
        let resource = self.gpu.create(q.width, q.height, device_format).await?;
        let source = Source::Imported {
            mapping,
            _memory: q.memory,
            start: q.offset - first,
            pitch: q.stride,
        };
        Ok(self.insert(resource, source, q.format))
    }

    /// Whether a CRTC shows `buffer`, or a commit in flight will.
    fn used(&self, buffer: u32) -> bool {
        let shown = self.crtcs.borrow().iter().any(|c| {
            c.shown.is_some_and(|s| s.buffer == buffer)
                || c.cursor.is_some_and(|s| s.buffer == buffer)
        });
        shown || self.showing.borrow().values().any(|b| b.contains(&buffer))
    }

    /// Marks `buffer` destroyed, and releases those destroyed buffers
    /// that nothing shows.
    fn destroy_buffer(self: &Rc<Self>, buffer: u32) {
        if let Some(b) = self.buffers.borrow_mut().get_mut(&buffer) {
            b.destroyed = true;
        }
        self.release_unused();
    }

    fn release_unused(self: &Rc<Self>) {
        let gone: Vec<Buffer> = {
            let mut buffers = self.buffers.borrow_mut();
            let ids: Vec<u32> = buffers
                .iter()
                .filter(|(id, b)| b.destroyed && !self.used(**id))
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| buffers.remove(&id))
                .collect()
        };
        for buffer in gone {
            let gpu = self.gpu.clone();
            self.spawner.spawn(async move {
                // Only the buffer held its resource.
                if let Ok(resource) = Rc::try_unwrap(buffer.resource) {
                    gpu.release(resource).await;
                }
            });
        }
    }

    /// Destroys every buffer and sync object of the closed connection,
    /// and turns the scanouts off but `log`'s, which the log is shown on
    /// next, and the cursors.
    pub fn closed(self: &Rc<Self>, log: Option<u32>) {
        for b in self.buffers.borrow_mut().values_mut() {
            b.destroyed = true;
        }
        self.syncs.borrow_mut().clear();
        let shown: Vec<usize> = self
            .crtcs
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, c)| c.shown.is_some() || c.cursor.is_some())
            .map(|(i, _)| i)
            .collect();
        for crtc in self.crtcs.borrow_mut().iter_mut() {
            *crtc = Crtc::default();
        }
        self.epoch.set(self.epoch.get() + 1);
        let display = self.clone();
        self.spawner.spawn(async move {
            for i in shown {
                if log != Some(i as u32) {
                    let off = protocol::set_scanout(i as u32, 0, protocol::Rect::default());
                    let _ = display.gpu.ok(&off).await;
                }
                let hide = protocol::update_cursor(i as u32, 0, 0, 0, (0, 0));
                let _ = display.gpu.cursor(&hide).await;
            }
            display.release_unused();
        });
    }
}

/// The device format of the fourcc code `format`.
fn device_format(format: u32) -> Option<u32> {
    match format {
        XRGB8888 => Some(FORMAT_B8G8R8X8),
        ARGB8888 => Some(FORMAT_B8G8R8A8),
        _ => None,
    }
}

fn copy_mode(m: &Mode) -> Mode {
    Mode {
        width: m.width,
        height: m.height,
        refresh_millihertz: m.refresh_millihertz,
        flags: m.flags,
    }
}

/// Serves the display owner's connection until it closes.
pub async fn serve(display: Rc<Display>, server: Rc<DisplayAsyncServer>) {
    *display.owner.borrow_mut() = Rc::downgrade(&server);
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            DisplayRequest::GetResources(_, r) => r.reply(
                s,
                DisplayGetResourcesResponse {
                    resources: display.resources(),
                },
            ),
            DisplayRequest::CreateDumbBuffer(q, r) => {
                let created = display.create_dumb(q.width, q.height, q.format).await;
                r.reply(
                    s,
                    created.map(|buffer| DisplayCreateDumbBufferResponse { buffer }),
                )
            }
            // The driver has no render part, so no token is its own.
            DisplayRequest::ImportBuffer(_, r) => r.reply(s, Err(Status::NotFound)),
            DisplayRequest::DestroyBuffer(q) => {
                display.destroy_buffer(q.buffer);
                Ok(())
            }
            DisplayRequest::ImportSync(q, r) => {
                let sync = display.id();
                display.syncs.borrow_mut().insert(sync, Rc::new(q.timeline));
                r.reply(s, Ok(DisplayImportSyncResponse { sync }))
            }
            DisplayRequest::DestroySync(q) => {
                display.syncs.borrow_mut().remove(&q.sync);
                Ok(())
            }
            DisplayRequest::Commit(q, r) => {
                let committed = display.commit(q.commit, &server);
                r.reply(
                    s,
                    committed.map(|commit_id| DisplayCommitResponse { commit_id }),
                )
            }
            DisplayRequest::ImportMemory(q, r) => {
                let imported = display.import_memory(q).await;
                r.reply(
                    s,
                    imported.map(|buffer| DisplayImportMemoryResponse { buffer }),
                )
            }
        };
    }
}
