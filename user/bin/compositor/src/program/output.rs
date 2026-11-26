//! A display that the compositor shows its frames on: the first
//! connected connector in its preferred mode, on the first CRTC with its
//! primary plane, and two output images of that size, which the display
//! imports from their memory objects and which frames are drawn into and
//! shown in turn. Each image keeps the damage of the frames since it was
//! last drawn (MIKSPEC-0066.R17), all of it until it was drawn once. When
//! the display has a cursor plane, the pointer can be shown on it
//! (MIKSPEC-0074.R20) and moved without drawing a frame.

use super::render::{Renderer, Target};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::gpu::{
    Colorspace, Commit, CommitFlags, CrtcState, DisplayAsyncClient, DisplayEvent, Mode, ModeFlags,
    PlaneState, PlaneType, Rect,
};
use mikros_rt::sys;

/// The DRM format of the output images, XRGB8888.
const XRGB8888: u32 = 0x3432_5258;
/// The DRM format of the cursor images, ARGB8888.
const ARGB8888: u32 = 0x3432_5241;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;

/// The display's cursor plane, and the two images that are shown on it in
/// turn: a new image is written into the one that is not shown.
struct CursorPlane {
    plane: u32,
    /// The most pixels across and down that the plane takes.
    limit: u32,
    images: [CursorImage; 2],
    /// The pointer image that each holds.
    holds: [Option<u64>; 2],
    /// What the display shows: the image and where its corner is.
    shown: Option<(usize, (i32, i32))>,
    /// What the next commit shows.
    want: Option<(usize, (i32, i32))>,
}

/// A cursor image on the display, mapped for writing. The mapping goes
/// with the image.
struct CursorImage {
    buffer: u32,
    pixels: *mut u32,
    /// Pixels per line.
    stride: usize,
    /// The process that maps it, and the bytes it maps.
    process: sys::Handle,
    len: u64,
}

impl Drop for CursorImage {
    fn drop(&mut self) {
        // SAFETY: the mapping was made by `cursor_plane`, and nothing
        // reads or writes the image once it is dropped.
        unsafe { sys::vm_unmap(self.process, self.pixels as u64, self.len) };
    }
}

/// The pointer image for the cursor plane: `size` pixels across and down
/// of premultiplied `0xAARRGGBB`, with the corner of its first pixel.
pub struct Want<'a> {
    /// Differs from that of any other pointer image.
    pub id: u64,
    pub pixels: &'a [u32],
    pub size: u32,
    pub corner: (i32, i32),
}

pub struct Output {
    display: DisplayAsyncClient,
    mode: Mode,
    connector: u32,
    crtc: u32,
    plane: u32,
    /// The output images, with their buffer IDs on the display.
    targets: Vec<(Target, u32)>,
    /// The image the next frame is drawn into.
    back: usize,
    /// What each image lacks of the frames since it was drawn: rectangles,
    /// or None for all of it.
    damage: Vec<Option<Vec<crate::server::Rect>>>,
    /// The cursor plane, when the display has one.
    cursor: Option<CursorPlane>,
    /// Whether the display may lack what a commit meant to show, so that
    /// the next one names all of the output as damaged.
    lost: bool,
}

/// The most damage rectangles that a plane state takes (MIKSPEC-0029.R3).
const DAMAGE_MAX: usize = 16;

impl Output {
    /// Takes `display` over, with output images that `renderer` makes, and
    /// the cursor images that the process `process` maps.
    pub async fn new(
        display: DisplayAsyncClient,
        renderer: &Renderer,
        process: sys::Handle,
    ) -> Result<Output, String> {
        let resources = display
            .get_resources()
            .await
            .map_err(|e| format!("GetResources: {e:?}"))?
            .resources;
        let connector = resources
            .connectors
            .iter()
            .find(|c| c.connected)
            .ok_or("no connected connector")?;
        let mode = connector
            .modes
            .iter()
            .find(|m| m.flags.contains(ModeFlags::PREFERRED))
            .or(connector.modes.first())
            .ok_or("no mode")?;
        let mode = Mode {
            width: mode.width,
            height: mode.height,
            refresh_millihertz: mode.refresh_millihertz,
            flags: mode.flags,
        };
        let (index, crtc) = resources
            .crtcs
            .iter()
            .enumerate()
            .find(|(i, _)| connector.possible_crtcs & (1 << i) != 0)
            .ok_or("no CRTC for the connector")?;
        let plane = resources
            .planes
            .iter()
            .find(|p| p.r#type == PlaneType::Primary && p.possible_crtcs & (1 << index) != 0)
            .ok_or("no primary plane")?
            .id;
        let cursor = Self::cursor_plane(&display, &resources, index, process).await;
        let mut targets = Vec::new();
        for _ in 0..2 {
            let (target, memory) = renderer
                .target(mode.width, mode.height)
                .map_err(|e| format!("an output image: {e:?}"))?;
            // SAFETY: the handle is ours, and the call takes it over.
            let memory = unsafe { OwnedHandle::from_raw(memory.into_raw_handle()) };
            let imported = display
                .import_memory(
                    memory,
                    mode.width,
                    mode.height,
                    XRGB8888,
                    target.stride,
                    target.offset,
                )
                .await;
            match imported {
                Ok(Ok(r)) => targets.push((target, r.buffer)),
                other => return Err(format!("ImportMemory: {other:?}")),
            }
        }
        Ok(Output {
            display,
            connector: connector.id,
            crtc: *crtc,
            plane,
            mode,
            damage: vec![None; targets.len()],
            targets,
            back: 0,
            cursor,
            lost: false,
        })
    }

    /// The cursor plane of the CRTC at `index` with its two images, when
    /// it takes `ARGB8888` and the images can be made and mapped.
    async fn cursor_plane(
        display: &DisplayAsyncClient,
        resources: &mikros_rt::interfaces::mikros::gpu::DisplayResources,
        index: usize,
        process: sys::Handle,
    ) -> Option<CursorPlane> {
        let plane = resources.planes.iter().find(|p| {
            p.r#type == PlaneType::Cursor
                && p.possible_crtcs & (1 << index) != 0
                && p.formats
                    .iter()
                    .any(|f| f.format == ARGB8888 && f.modifier == 0)
        })?;
        let limit = plane.max_width.min(plane.max_height);
        let mut images = Vec::new();
        for _ in 0..2 {
            let made = display.create_dumb_buffer(limit, limit, ARGB8888).await;
            let Ok(Ok(r)) = made else {
                mikros_rt::log!(Warning, "compositor: no cursor image: {made:?}");
                return None;
            };
            let buffer = r.buffer;
            // The lines of the image must lie in the memory that maps it,
            // which the display tells and this does not take on trust.
            let wide = u64::from(buffer.stride) >= u64::from(limit) * 4;
            let fits = u64::from(buffer.stride) * u64::from(limit) <= buffer.size;
            if !wide || !fits {
                mikros_rt::log!(
                    Warning,
                    "compositor: a cursor image of {limit} pixels does not fit its {} bytes at stride {}",
                    buffer.size,
                    buffer.stride
                );
                return None;
            }
            let mut address = 0;
            // SAFETY: the out pointer is valid for the call.
            let status = unsafe {
                sys::vm_map(
                    process,
                    PERM_READ | PERM_WRITE,
                    0,
                    buffer.memory.raw(),
                    0,
                    buffer.size,
                    &mut address,
                )
            };
            if status != mikros_rt::OK {
                mikros_rt::log!(Warning, "compositor: mapping a cursor image: {status}");
                return None;
            }
            images.push(CursorImage {
                buffer: buffer.buffer,
                pixels: address as *mut u32,
                stride: buffer.stride as usize / 4,
                process,
                len: buffer.size,
            });
        }
        let [first, second]: [CursorImage; 2] = images.try_into().ok()?;
        mikros_rt::log!(
            Info,
            "compositor: the pointer goes on the cursor plane of the display, {limit} pixels across at most"
        );
        Some(CursorPlane {
            plane: plane.id,
            limit,
            images: [first, second],
            holds: [None, None],
            shown: None,
            want: None,
        })
    }

    /// The most pixels across that a pointer image for the cursor plane
    /// may have, or none without a cursor plane.
    pub fn cursor_limit(&self) -> Option<u32> {
        self.cursor.as_ref().map(|c| c.limit)
    }

    /// Chooses what the next commit shows on the cursor plane: the
    /// pointer image `want`, written into the image that is not shown
    /// unless one holds it already, or nothing.
    pub fn set_cursor(&mut self, want: Option<Want<'_>>) {
        let Some(cursor) = &mut self.cursor else {
            return;
        };
        let Some(want) = want else {
            cursor.want = None;
            return;
        };
        let at = match cursor.holds.iter().position(|h| *h == Some(want.id)) {
            Some(at) => at,
            None => {
                let at = match cursor.shown {
                    Some((shown, _)) => 1 - shown,
                    None => 0,
                };
                let image = &cursor.images[at];
                let limit = cursor.limit as usize;
                let size = (want.size as usize).min(limit);
                for y in 0..limit {
                    for x in 0..limit {
                        let pixel = match x < size && y < size {
                            true => want.pixels[y * want.size as usize + x],
                            false => 0,
                        };
                        // SAFETY: the pixel lies within the mapped image.
                        unsafe { image.pixels.add(y * image.stride + x).write_volatile(pixel) };
                    }
                }
                cursor.holds[at] = Some(want.id);
                at
            }
        };
        cursor.want = Some((at, want.corner));
    }

    /// The cursor plane's state in a commit: the wanted image, or the
    /// plane off when something is shown on it, or nothing to change.
    fn cursor_state(&self) -> Option<PlaneState> {
        let cursor = self.cursor.as_ref()?;
        let size = Rect {
            x: 0,
            y: 0,
            width: cursor.limit,
            height: cursor.limit,
        };
        match (cursor.want, cursor.shown) {
            (Some((image, (x, y))), _) => Some(PlaneState {
                plane: cursor.plane,
                crtc: self.crtc,
                buffer: cursor.images[image].buffer,
                src: Rect { ..size },
                dst: Rect { x, y, ..size },
                damage: Vec::new(),
                acquire_sync: 0,
                acquire_value: 0,
            }),
            (None, Some(_)) => Some(PlaneState {
                plane: cursor.plane,
                crtc: 0,
                buffer: 0,
                src: Rect { ..size },
                dst: Rect { ..size },
                damage: Vec::new(),
                acquire_sync: 0,
                acquire_value: 0,
            }),
            (None, None) => None,
        }
    }

    /// Shows the wanted cursor alone, without a frame, when the display
    /// does not show it already.
    pub async fn show_cursor(&mut self) -> Result<(), Status> {
        let Some(cursor) = &self.cursor else {
            return Ok(());
        };
        if cursor.want == cursor.shown {
            return Ok(());
        }
        let Some(state) = self.cursor_state() else {
            return Ok(());
        };
        let commit = Commit {
            flags: CommitFlags(0),
            crtcs: Vec::new(),
            planes: vec![state],
        };
        self.commit(commit).await?;
        if let Some(cursor) = &mut self.cursor {
            cursor.shown = cursor.want;
        }
        Ok(())
    }

    /// Commits `commit` and returns once the display shows it.
    async fn commit(&self, commit: Commit) -> Result<(), Status> {
        let id = match self.display.commit(commit).await {
            Ok(Ok(r)) => r.commit_id,
            Ok(Err(status)) => return Err(status),
            Err(_) => return Err(Status::PeerClosed),
        };
        loop {
            match self.display.next_event().await {
                Ok(DisplayEvent::OnCommitComplete(c)) if c.commit_id == id => return Ok(()),
                Ok(_) => {}
                Err(_) => return Err(Status::PeerClosed),
            }
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.mode.width, self.mode.height)
    }

    /// The image to draw the next frame into.
    pub fn back(&self) -> &Target {
        &self.targets[self.back].0
    }

    /// Adds a frame's damage, `rects` or all of the output when `whole`,
    /// to that of each image.
    /// The image that the last frame was drawn into does not hold it all
    /// (the drawing failed): the next frame draws all of it and names all
    /// of the output as damaged.
    pub fn invalidate(&mut self) {
        self.damage[self.back] = None;
        self.lost = true;
    }

    pub fn add_damage(&mut self, rects: &[crate::server::Rect], whole: bool) {
        for damage in &mut self.damage {
            match (damage, whole) {
                (d, true) => *d = None,
                (Some(d), false) => d.extend_from_slice(rects),
                (None, false) => {}
            }
        }
    }

    /// What the image to draw the next frame into lacks: rectangles, or
    /// None for all of it.
    pub fn back_damage(&self) -> Option<Vec<crate::server::Rect>> {
        self.damage[self.back].clone()
    }

    /// Shows the image that the last frame was drawn into, which then
    /// lacks nothing, and returns once the display shows it. `changed` is
    /// what the frame changed, or None for all of the output.
    pub async fn show(&mut self, changed: Option<&[crate::server::Rect]>) -> Result<(), Status> {
        let (width, height) = self.size();
        let whole = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let damage = match changed {
            None => vec![Rect { ..whole }],
            // Everything, when the display may lack an earlier commit.
            Some(_) if self.lost => vec![Rect { ..whole }],
            Some(rects) => crate::server::join_damage(rects, DAMAGE_MAX)
                .iter()
                .filter_map(|r| {
                    let (x0, y0) = (r.x.max(0) as u32, r.y.max(0) as u32);
                    let x1 = (r.x.saturating_add(r.width).max(0) as u32).min(width);
                    let y1 = (r.y.saturating_add(r.height).max(0) as u32).min(height);
                    (x1 > x0 && y1 > y0).then_some(Rect {
                        x: x0 as i32,
                        y: y0 as i32,
                        width: x1 - x0,
                        height: y1 - y0,
                    })
                })
                .collect(),
        };
        self.damage[self.back] = Some(Vec::new());
        let mut planes = vec![PlaneState {
            plane: self.plane,
            crtc: self.crtc,
            buffer: self.targets[self.back].1,
            src: Rect { ..whole },
            dst: Rect { ..whole },
            damage,
            acquire_sync: 0,
            acquire_value: 0,
        }];
        planes.extend(self.cursor_state());
        let commit = Commit {
            flags: CommitFlags(0),
            crtcs: vec![CrtcState {
                crtc: self.crtc,
                active: true,
                connector: self.connector,
                mode: Mode { ..self.mode },
                vrr: false,
                gamma: None,
                colorspace: Colorspace::Default,
                hdr: None,
            }],
            planes,
        };
        if let Err(status) = self.commit(commit).await {
            // What the commit meant to show may not be there.
            self.lost = true;
            return Err(status);
        }
        self.lost = false;
        if let Some(cursor) = &mut self.cursor {
            cursor.shown = cursor.want;
        }
        self.back = (self.back + 1) % self.targets.len();
        Ok(())
    }
}
