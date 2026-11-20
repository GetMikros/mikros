//! The buffers and timelines of clients: buffers imported into the
//! renderer, and kernel timelines kept as handles, whose release points
//! the compositor reaches with the timeline system calls.

use super::render::{ClientBuffer, Renderer};
use crate::server::{BufferId, Imports, Layout, Memory, Point, TimelineId};
use crate::wayland::wire::Fd;
use std::collections::BTreeMap;

/// The DRM formats of the buffers that the renderer copies, and the
/// linear modifier, which is the only one.
pub const FORMATS: [(u32, u64); 2] = [(XRGB8888, LINEAR), (ARGB8888, LINEAR)];
const XRGB8888: u32 = 0x3432_5258;
const ARGB8888: u32 = 0x3432_5241;
const LINEAR: u64 = 0;
/// The kernel's object type of a timeline.
const OBJECT_TIMELINE: u32 = 14;

/// The imported buffers and timelines, by the IDs the server knows them by,
/// and the copy of the output's wallpaper.
#[derive(Default)]
pub struct Store {
    pub buffers: BTreeMap<BufferId, ClientBuffer>,
    pub wallpaper: Option<ClientBuffer>,
    timelines: BTreeMap<TimelineId, Fd>,
    next: u64,
}

impl Store {
    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    /// The handle of the timeline `id`, while the server knows it.
    pub fn timeline(&self, id: TimelineId) -> Option<u32> {
        self.timelines.get(&id).map(|t| t.as_raw_handle())
    }

    /// Reaches `point`, a release point of a buffer that no frame reads
    /// any more.
    pub fn reach(&self, point: Point) {
        if let Some(timeline) = self.timeline(point.timeline) {
            // SAFETY: the call takes no pointers. A value below the
            // timeline's, which a client could have signalled, fails
            // harmlessly.
            let _ = unsafe { mikros_rt::sys::timeline_signal(timeline, point.value) };
        }
    }
}

/// The store, with the renderer that imports buffers into it.
pub struct Importer<'a> {
    pub renderer: &'a Renderer,
    pub store: &'a mut Store,
}

impl Imports for Importer<'_> {
    fn buffer(&mut self, memory: Memory, handle: Fd, layout: Layout) -> Option<BufferId> {
        if !FORMATS.contains(&(layout.format, layout.modifier)) {
            return None;
        }
        let shape = (layout.width, layout.height, layout.stride, layout.offset);
        let buffer = self
            .renderer
            .import(handle, memory == Memory::Token, shape)
            .ok()?;
        let id = self.store.id();
        self.store.buffers.insert(id, buffer);
        Some(id)
    }

    fn timeline(&mut self, handle: Fd) -> Option<TimelineId> {
        let mut record = [0u8; 24];
        let len = mikros_rt::objects::info(handle.as_raw_handle(), 1, &mut record).ok()?;
        let kind = u32::from_ne_bytes(record[16..20].try_into().expect("four bytes"));
        if len < record.len() || kind != OBJECT_TIMELINE {
            return None;
        }
        let id = self.store.id();
        self.store.timelines.insert(id, handle);
        Some(id)
    }

    fn drop_buffer(&mut self, buffer: BufferId) {
        if let Some(buffer) = self.store.buffers.remove(&buffer) {
            self.renderer.forget(buffer);
        }
    }

    fn drop_timeline(&mut self, timeline: TimelineId) {
        self.store.timelines.remove(&timeline);
    }

    fn set_wallpaper(&mut self, buffer: BufferId) -> bool {
        let Some(source) = self.store.buffers.get(&buffer) else {
            return false;
        };
        match self.renderer.copy(source) {
            Ok(copy) => {
                if let Some(old) = self.store.wallpaper.replace(copy) {
                    self.renderer.forget(old);
                }
                true
            }
            Err(e) => {
                mikros_rt::log!(Error, "compositor: copying the wallpaper: {e:?}");
                false
            }
        }
    }
}
