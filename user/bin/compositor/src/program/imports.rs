//! The buffers and timelines of clients: buffers imported into the
//! renderer, and kernel timelines kept as handles, whose release points
//! the compositor reaches with the timeline system calls. The store also
//! holds the output's wallpaper, its pixels and a buffer of them that the
//! renderer draws, and its haze (MIKSPEC-0066.R18), made again whenever
//! the wallpaper or the theme's scheme changes, with the one before it
//! while the new one fades in.

use super::host::HostBuffer;
use super::render::{ClientBuffer, Renderer};
use crate::server::{BufferId, Imports, Layout, Memory, Point, TimelineId};
use crate::theme::Scheme;
use crate::wayland::wire::Fd;
use std::collections::BTreeMap;

/// The DRM formats of the buffers that the renderer draws, and the
/// linear modifier, which is the only one.
pub const FORMATS: [(u32, u64); 2] = [(XRGB8888, LINEAR), (ARGB8888, LINEAR)];
const XRGB8888: u32 = 0x3432_5258;
const ARGB8888: u32 = 0x3432_5241;
const LINEAR: u64 = 0;
/// The kernel's object type of a timeline.
const OBJECT_TIMELINE: u32 = 14;

/// The imported buffers and timelines, by the IDs the server knows them by,
/// and the output's wallpaper and haze.
#[derive(Default)]
pub struct Store {
    pub buffers: BTreeMap<BufferId, ClientBuffer>,
    pub wallpaper: Option<Wallpaper>,
    pub haze: Option<Haze>,
    /// The theme's scheme, which tints the haze.
    pub scheme: Scheme,
    /// Whether the haze changed, which changes all of the output.
    pub damaged_all: bool,
    timelines: BTreeMap<TimelineId, Fd>,
    next: u64,
}

pub struct Wallpaper {
    pixels: Vec<u32>,
    pub buffer: HostBuffer,
}

/// The output's haze, the one before it while the new one fades in, and
/// when the new one was made, in nanoseconds of the monotonic clock.
pub struct Haze {
    pub new: HostBuffer,
    pub old: Option<HostBuffer>,
    pub made: i64,
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

    /// Makes the haze of the wallpaper again, with the scheme, keeping the
    /// last one to fade from.
    pub fn remake_haze(&mut self, renderer: &Renderer) {
        let Some(wallpaper) = &self.wallpaper else {
            return;
        };
        let (width, height) = (wallpaper.buffer.width, wallpaper.buffer.height);
        let (w, h) = (width as usize, height as usize);
        let pixels = crate::haze::haze(&wallpaper.pixels, w, h, w, self.scheme);
        let new = match renderer.host_pixels(&pixels, width, height) {
            Ok(new) => new,
            Err(e) => {
                mikros_rt::log!(Error, "compositor: making the haze: {e:?}");
                return;
            }
        };
        let made = mikros_rt::objects::now();
        let old = match self.haze.take() {
            Some(haze) => {
                if let Some(older) = haze.old {
                    renderer.forget_host(older);
                }
                Some(haze.new)
            }
            None => None,
        };
        self.haze = Some(Haze { new, old, made });
        self.damaged_all = true;
    }

    /// Forgets the haze before the new one once that one has faded in.
    pub fn faded_in(&mut self, renderer: &Renderer) {
        if let Some(old) = self.haze.as_mut().and_then(|h| h.old.take()) {
            renderer.forget_host(old);
        }
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
        let alpha = layout.format == ARGB8888;
        let buffer = self
            .renderer
            .import(handle, memory == Memory::Token, shape, alpha)
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

    fn memory(&mut self, bytes: &[u8]) -> Option<Fd> {
        let size = (bytes.len() as u64).next_multiple_of(4096);
        let memory = mikros_rt::objects::memory(size, 0).ok()?;
        mikros_rt::objects::write_memory(memory.raw(), 0, bytes).ok()?;
        // SAFETY: the handle is ours, and is handed over once.
        Some(unsafe { Fd::from_raw_handle(memory.into_raw()) })
    }

    fn set_wallpaper(&mut self, buffer: BufferId) -> bool {
        let Some(source) = self.store.buffers.get(&buffer) else {
            return false;
        };
        let (width, height) = (source.width, source.height);
        let copied = self.renderer.read(source).and_then(|pixels| {
            let buffer = self.renderer.host_pixels(&pixels, width, height)?;
            Ok(Wallpaper { pixels, buffer })
        });
        match copied {
            Ok(wallpaper) => {
                if let Some(old) = self.store.wallpaper.replace(wallpaper) {
                    self.renderer.forget_host(old.buffer);
                }
                self.store.remake_haze(self.renderer);
                true
            }
            Err(e) => {
                mikros_rt::log!(Error, "compositor: copying the wallpaper: {e:?}");
                false
            }
        }
    }
}
