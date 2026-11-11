//! The `Display` connection of the display owner: the resources, dumb
//! buffers, sync objects, and commits presented by copying the plane's
//! buffer into the framebuffer; and what the gpudev library needs of the
//! device.

use crate::framebuffer::{Framebuffer, PERM_READ};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::Future;
use mikros_display::rects::{Rect, copied};
use mikros_gpudev::Backend;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::gpu::{
    self as gpu, Capabilities, Colorspace, Commit, CommitFlags, Connector, ConnectorCaps,
    ConnectorType, DeviceInfo, DisplayAsyncServer, DisplayCommitResponse,
    DisplayCreateDumbBufferResponse, DisplayFlags, DisplayGetResourcesResponse,
    DisplayImportSyncResponse, DisplayRequest, DisplayResources, DumbBuffer, FormatModifier, Mode,
    ModeFlags, Plane, PlaneType, Render,
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
/// The shortest time between two presented commits, in nanoseconds.
const FRAME: i64 = 1_000_000_000 / 60;
const PAGE_SIZE: u64 = 4096;
const SAME_RIGHTS: u32 = 1 << 31;

/// A dumb buffer, mapped for reading.
struct Buffer {
    _memory: OwnedHandle,
    address: u64,
    len: u64,
    width: u32,
    height: u32,
    /// Bytes per line.
    pitch: u32,
    /// Whether the owner destroyed it; it is freed once nothing shows it.
    destroyed: bool,
}

/// What a plane shows: a buffer, the part of it, and where.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Shown {
    buffer: u32,
    src: Rect,
    dst: Rect,
}

/// The display's state, shared by the connection and its presenting.
pub struct Display {
    pub framebuffer: RefCell<Option<Framebuffer>>,
    process: sys::Handle,
    reactor: Reactor,
    spawner: Spawner,
    buffers: RefCell<BTreeMap<u32, Buffer>>,
    syncs: RefCell<BTreeMap<u32, Rc<OwnedHandle>>>,
    next_id: Cell<u32>,
    next_commit: Cell<u64>,
    active: Cell<bool>,
    shown: Cell<Option<Shown>>,
    /// The commit being presented, and what it will show.
    pending: Cell<Option<(u64, Option<Shown>)>>,
    last_present: Cell<i64>,
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

fn rect(r: &gpu::Rect) -> Rect {
    Rect::new(
        i64::from(r.x),
        i64::from(r.y),
        u64::from(r.width),
        u64::from(r.height),
    )
}

impl Display {
    pub fn new(
        framebuffer: Framebuffer,
        process: sys::Handle,
        reactor: Reactor,
        spawner: Spawner,
    ) -> Display {
        Display {
            framebuffer: RefCell::new(Some(framebuffer)),
            process,
            reactor,
            spawner,
            buffers: RefCell::new(BTreeMap::new()),
            syncs: RefCell::new(BTreeMap::new()),
            next_id: Cell::new(1),
            next_commit: Cell::new(1),
            active: Cell::new(true),
            shown: Cell::new(None),
            pending: Cell::new(None),
            last_present: Cell::new(i64::MIN / 2),
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
                width,
                height,
                pitch,
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

    /// Marks `buffer` destroyed, and frees it unless a plane shows it or
    /// will.
    fn destroy_buffer(&self, buffer: u32) {
        if let Some(b) = self.buffers.borrow_mut().get_mut(&buffer) {
            b.destroyed = true;
        }
        self.free_unused();
    }

    fn free_unused(&self) {
        let used = |id: u32| {
            self.shown.get().is_some_and(|s| s.buffer == id)
                || matches!(self.pending.get(), Some((_, Some(s))) if s.buffer == id)
        };
        let mut buffers = self.buffers.borrow_mut();
        let gone: Vec<u32> = buffers
            .iter()
            .filter(|(id, b)| b.destroyed && !used(**id))
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            if let Some(b) = buffers.remove(&id) {
                // SAFETY: nothing reads the buffer any more.
                unsafe { sys::vm_unmap(self.process, b.address, b.len) };
            }
        }
    }

    /// Checks `commit` against the one mode and plane: what the plane
    /// will show, whether the CRTC is active, the damage, and the sync
    /// object and value to wait for.
    #[allow(clippy::type_complexity)]
    fn check(
        &self,
        commit: &Commit,
    ) -> Result<
        (
            bool,
            Option<Shown>,
            Vec<Rect>,
            Option<(Rc<OwnedHandle>, u64)>,
        ),
        Status,
    > {
        if commit.flags.contains(CommitFlags::ASYNC) {
            return Err(Status::InvalidArgs);
        }
        let mut active = self.active.get();
        for crtc in &commit.crtcs {
            let mode = self.mode();
            let wrong_mode = crtc.mode.width != mode.width
                || crtc.mode.height != mode.height
                || crtc.mode.refresh_millihertz != mode.refresh_millihertz;
            let extras = crtc.vrr
                || crtc.gamma.is_some()
                || crtc.hdr.is_some()
                || crtc.colorspace != Colorspace::Default;
            if crtc.crtc != CRTC
                || extras
                || (crtc.active && (crtc.connector != CONNECTOR || wrong_mode))
            {
                return Err(Status::InvalidArgs);
            }
            active = crtc.active;
        }
        let mut shown = self.shown.get();
        let mut damage = Vec::new();
        let mut sync = None;
        for plane in &commit.planes {
            if plane.plane != PLANE {
                return Err(Status::InvalidArgs);
            }
            if plane.crtc == 0 {
                shown = None;
                continue;
            }
            let (src, dst) = (rect(&plane.src), rect(&plane.dst));
            let buffers = self.buffers.borrow();
            let buffer = buffers.get(&plane.buffer).filter(|b| !b.destroyed);
            let (width, height) = self.size();
            let fits = buffer.is_some_and(|b| {
                Rect::new(0, 0, u64::from(b.width), u64::from(b.height)).contains(&src)
            });
            let on_screen = Rect::new(0, 0, u64::from(width), u64::from(height)).contains(&dst);
            let same_size = src.width == dst.width && src.height == dst.height;
            if plane.crtc != CRTC
                || !fits
                || !on_screen
                || !same_size
                || src.width == 0
                || src.height == 0
            {
                return Err(Status::InvalidArgs);
            }
            if plane.acquire_sync != 0 {
                let syncs = self.syncs.borrow();
                let timeline = syncs.get(&plane.acquire_sync).ok_or(Status::InvalidArgs)?;
                sync = Some((timeline.clone(), plane.acquire_value));
            }
            shown = Some(Shown {
                buffer: plane.buffer,
                src,
                dst,
            });
            damage = plane.damage.iter().map(rect).collect();
        }
        if shown.is_some() && !active {
            return Err(Status::InvalidArgs);
        }
        Ok((active, shown, damage, sync))
    }

    fn commit(
        self: &Rc<Self>,
        commit: Commit,
        owner: &Rc<DisplayAsyncServer>,
    ) -> Result<u64, Status> {
        let (active, shown, damage, sync) = self.check(&commit)?;
        let test_only = commit.flags.contains(CommitFlags::TEST_ONLY);
        if !test_only && self.pending.get().is_some() {
            return Err(Status::ShouldWait);
        }
        let id = self.next_commit.get();
        self.next_commit.set(id + 1);
        if test_only {
            return Ok(id);
        }
        self.pending.set(Some((id, shown)));
        self.active.set(active);
        let present = self
            .clone()
            .present(id, shown, damage, sync, Rc::downgrade(owner));
        self.spawner.spawn(present);
        Ok(id)
    }

    /// Presents commit `id` once its sync object reached its value and a
    /// frame passed since the last one.
    async fn present(
        self: Rc<Self>,
        id: u64,
        shown: Option<Shown>,
        damage: Vec<Rect>,
        sync: Option<(Rc<OwnedHandle>, u64)>,
        owner: alloc::rc::Weak<DisplayAsyncServer>,
    ) {
        if let Some((timeline, value)) = sync {
            let _ = self.reactor.timeline(timeline.raw(), value).await;
        }
        let earliest = self.last_present.get().saturating_add(FRAME);
        if now() < earliest {
            let _ = self.reactor.sleep_until(earliest).await;
        }
        let changed = shown != self.shown.get();
        if let Some(fb) = self.framebuffer.borrow().as_ref() {
            match shown {
                Some(s) => {
                    // A new buffer or place is copied whole.
                    let damage = if changed { &[][..] } else { &damage[..] };
                    self.copy(fb, &s, damage);
                }
                None => clear(fb),
            }
        }
        let done = now();
        self.last_present.set(done);
        self.shown.set(shown);
        self.pending.set(None);
        self.free_unused();
        if let Some(owner) = owner.upgrade() {
            let _ = owner.server().send_on_commit_complete(id, done);
        }
    }

    fn copy(&self, fb: &Framebuffer, shown: &Shown, damage: &[Rect]) {
        let buffers = self.buffers.borrow();
        let Some(buffer) = buffers.get(&shown.buffer) else {
            return;
        };
        for r in copied(&shown.src, damage) {
            let (dx, dy) = (shown.dst.x - shown.src.x, shown.dst.y - shown.src.y);
            for row in 0..r.height {
                let y = r.y as u64 + row;
                let line = buffer.address + y * u64::from(buffer.pitch) + r.x as u64 * 4;
                let pixels = (0..r.width).map(|i| {
                    // SAFETY: `check` kept the source within the buffer.
                    unsafe { ((line + i * 4) as *const u32).read_volatile() }
                });
                fb.write_row((r.x + dx) as u32, (y as i64 + dy) as u32, pixels);
            }
        }
    }
}

impl Backend for Display {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            name: "Basic Display".into(),
            driver: "basicdisplay".into(),
            driver_version: 1,
            vendor_id: 0,
            device_id: 0,
            capabilities: Capabilities::DISPLAY,
        }
    }

    fn serve(
        self: Rc<Self>,
        display: Rc<DisplayAsyncServer>,
    ) -> impl Future<Output = ()> + 'static {
        serve(self, display)
    }

    /// Destroys every buffer and sync object of the closed connection.
    /// The shown buffer stays shown until the next owner commits.
    fn closed(&self) {
        for b in self.buffers.borrow_mut().values_mut() {
            b.destroyed = true;
        }
        self.syncs.borrow_mut().clear();
        self.free_unused();
    }

    /// Without `Render` connections no buffer can be exported.
    fn open_render(&self, _render: ServerEnd<Render>) -> Result<(), Status> {
        Err(Status::NotSupported)
    }
}

/// Fills the framebuffer with black.
fn clear(fb: &Framebuffer) {
    let d = fb.description;
    for y in 0..d.height {
        fb.write_row(0, y, core::iter::repeat_n(0, d.width as usize));
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
            DisplayRequest::Commit(q, r) => {
                let committed = display.commit(q.commit, &server);
                r.reply(
                    s,
                    committed.map(|commit_id| DisplayCommitResponse { commit_id }),
                )
            }
        };
    }
}
