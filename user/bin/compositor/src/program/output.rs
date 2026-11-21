//! A display that the compositor shows its frames on: the first
//! connected connector in its preferred mode, on the first CRTC with its
//! primary plane, and two output images of that size, which the display
//! imports from their memory objects and which frames are drawn into and
//! shown in turn. Each image keeps the damage of the frames since it was
//! last drawn (MIKSPEC-0066.R17), all of it until it was drawn once.

use super::render::{Renderer, Target};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::gpu::{
    Colorspace, Commit, CommitFlags, CrtcState, DisplayAsyncClient, DisplayEvent, Mode, ModeFlags,
    PlaneState, PlaneType, Rect,
};

/// The DRM format of the output images, XRGB8888.
const XRGB8888: u32 = 0x3432_5258;

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
}

impl Output {
    /// Takes `display` over, with output images that `renderer` makes.
    pub async fn new(display: DisplayAsyncClient, renderer: &Renderer) -> Result<Output, String> {
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
        })
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
            Some(rects) => rects
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
            planes: vec![PlaneState {
                plane: self.plane,
                crtc: self.crtc,
                buffer: self.targets[self.back].1,
                src: Rect { ..whole },
                dst: Rect { ..whole },
                damage,
                acquire_sync: 0,
                acquire_value: 0,
            }],
        };
        let id = match self.display.commit(commit).await {
            Ok(Ok(r)) => r.commit_id,
            Ok(Err(status)) => return Err(status),
            Err(_) => return Err(Status::PeerClosed),
        };
        loop {
            match self.display.next_event().await {
                Ok(DisplayEvent::OnCommitComplete(c)) if c.commit_id == id => break,
                Ok(_) => {}
                Err(_) => return Err(Status::PeerClosed),
            }
        }
        self.back = (self.back + 1) % self.targets.len();
        Ok(())
    }
}
