//! The `Display` connection of the display owner: the resources, dumb
//! buffers, sync objects, and commits presented by copying the plane's
//! buffer into the framebuffer.

use crate::framebuffer::{Framebuffer, PERM_READ};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::gpu::{
    Connector, ConnectorCaps, ConnectorType, DisplayAsyncServer, DisplayCreateDumbBufferResponse,
    DisplayFlags, DisplayGetResourcesResponse, DisplayImportSyncResponse, DisplayRequest,
    DisplayResources, DumbBuffer, FormatModifier, Mode, ModeFlags, Plane, PlaneType,
};
use mikros_rt::sys;

/// The IDs of the one connector, CRTC and plane.
const CONNECTOR: u32 = 1;
const CRTC: u32 = 1;
const PLANE: u32 = 1;
/// The bit of the CRTC in `possible_crtcs`.
const CRTC_BIT: u32 = 1;
/// XRGB8888's fourcc code, and the linear modifier.
pub const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
const LINEAR: u64 = 0;
const REFRESH_MILLIHERTZ: u32 = 60_000;
const PAGE_SIZE: u64 = 4096;
const SAME_RIGHTS: u32 = 1 << 31;

/// A dumb buffer, mapped for reading.
struct Buffer {
    _memory: OwnedHandle,
    address: u64,
    len: u64,
    /// Whether the owner destroyed it; it is freed once nothing shows it.
    destroyed: bool,
}

/// The display's state, shared by the connection and its presenting.
pub struct Display {
    pub framebuffer: RefCell<Option<Framebuffer>>,
    process: sys::Handle,
    buffers: RefCell<BTreeMap<u32, Buffer>>,
    syncs: RefCell<BTreeMap<u32, Rc<OwnedHandle>>>,
    next_id: Cell<u32>,
}

impl Display {
    pub fn new(framebuffer: Framebuffer, process: sys::Handle) -> Display {
        Display {
            framebuffer: RefCell::new(Some(framebuffer)),
            process,
            buffers: RefCell::new(BTreeMap::new()),
            syncs: RefCell::new(BTreeMap::new()),
            next_id: Cell::new(1),
        }
    }

    fn size(&self) -> (u32, u32) {
        let fb = self.framebuffer.borrow();
        fb.as_ref()
            .map_or((0, 0), |f| (f.description.width, f.description.height))
    }

    fn mode(&self) -> Mode {
        let (width, height) = self.size();
        Mode {
            width,
            height,
            refresh_millihertz: REFRESH_MILLIHERTZ,
            flags: ModeFlags::PREFERRED,
        }
    }

    fn id(&self) -> u32 {
        let id = self.next_id.get();
        self.next_id.set(id.wrapping_add(1).max(1));
        id
    }

    fn resources(&self) -> DisplayResources {
        let (width, height) = self.size();
        DisplayResources {
            flags: DisplayFlags(0),
            connectors: vec![Connector {
                id: CONNECTOR,
                r#type: ConnectorType::BootFramebuffer,
                connected: true,
                possible_crtcs: CRTC_BIT,
                caps: ConnectorCaps(0),
                gamma_size: 0,
                modes: vec![self.mode()],
                edid: Vec::new(),
            }],
            crtcs: vec![CRTC],
            planes: vec![Plane {
                id: PLANE,
                r#type: PlaneType::Primary,
                possible_crtcs: CRTC_BIT,
                max_width: width,
                max_height: height,
                formats: vec![FormatModifier {
                    format: XRGB8888,
                    modifier: LINEAR,
                }],
            }],
        }
    }

    fn create_dumb(&self, width: u32, height: u32, format: u32) -> Result<DumbBuffer, Status> {
        let (max_width, max_height) = self.size();
        if format != XRGB8888 || width == 0 || height == 0 {
            return Err(Status::InvalidArgs);
        }
        if width > max_width || height > max_height {
            return Err(Status::OutOfRange);
        }
        let pitch = width * 4;
        let size = (u64::from(pitch) * u64::from(height)).next_multiple_of(PAGE_SIZE);
        let mut memory = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::mem_create(size, 0, &mut memory) };
        if status != mikros_rt::OK {
            return Err(Status::NoMemory);
        }
        // SAFETY: the kernel just created the handle for us.
        let memory = unsafe { OwnedHandle::from_raw(memory) };
        let mut address = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe {
            sys::vm_map(
                self.process,
                PERM_READ,
                0,
                memory.raw(),
                0,
                size,
                &mut address,
            )
        };
        if status != mikros_rt::OK {
            return Err(Status::NoMemory);
        }
        let mut client = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::handle_duplicate(memory.raw(), SAME_RIGHTS, &mut client) };
        if status != mikros_rt::OK {
            // SAFETY: nothing refers to the mapping yet.
            unsafe { sys::vm_unmap(self.process, address, size) };
            return Err(Status::NoResources);
        }
        let buffer = self.id();
        self.buffers.borrow_mut().insert(
            buffer,
            Buffer {
                _memory: memory,
                address,
                len: size,
                destroyed: false,
            },
        );
        Ok(DumbBuffer {
            buffer,
            stride: pitch,
            size,
            // SAFETY: the kernel just created the handle for us.
            memory: unsafe { OwnedHandle::from_raw(client) },
        })
    }

    /// Destroys `buffer`.
    fn destroy_buffer(&self, buffer: u32) {
        if let Some(b) = self.buffers.borrow_mut().get_mut(&buffer) {
            b.destroyed = true;
        }
        self.free_unused();
    }

    fn free_unused(&self) {
        let mut buffers = self.buffers.borrow_mut();
        let gone: Vec<u32> = buffers
            .iter()
            .filter(|(_, b)| b.destroyed)
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            if let Some(b) = buffers.remove(&id) {
                // SAFETY: nothing reads the buffer any more.
                unsafe { sys::vm_unmap(self.process, b.address, b.len) };
            }
        }
    }

    /// Destroys every buffer and sync object of the closed connection.
    pub fn closed(&self) {
        for b in self.buffers.borrow_mut().values_mut() {
            b.destroyed = true;
        }
        self.syncs.borrow_mut().clear();
        self.free_unused();
    }
}

/// Serves the display owner's connection until it closes.
pub async fn serve(display: Rc<Display>, server: Rc<DisplayAsyncServer>) {
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
                let created = display.create_dumb(q.width, q.height, q.format);
                r.reply(
                    s,
                    created.map(|buffer| DisplayCreateDumbBufferResponse { buffer }),
                )
            }
            // Without `Render` connections no buffer can be exported.
            DisplayRequest::ImportBuffer(_, r) => r.reply(s, Err(Status::NotSupported)),
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
            // Commits come with presenting, in the next change.
            DisplayRequest::Commit(_, r) => r.reply(s, Err(Status::NotSupported)),
        };
    }
}
