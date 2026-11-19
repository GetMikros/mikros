//! Commits: checking them against the outputs, and presenting them by
//! setting the scanouts, copying what an imported buffer shows into its
//! resource and the damaged parts to the host, flushing them with a
//! fence on the last command, and showing the cursors.

use super::{ARGB8888, CURSOR, Change, Crtc, Cursor, Display, Shown, Source, cursor_plane};
use crate::driver::gpu::Resource;
use crate::protocol;
use alloc::rc::{Rc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use mikros_display::rects::{Rect, copied};
use mikros_rt::hardware::Mapping;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::gpu::{
    self as gpu, Colorspace, Commit, CommitFlags, DisplayAsyncServer,
};
use mikros_rt::objects::now;

fn rect(r: &gpu::Rect) -> Rect {
    Rect::new(
        i64::from(r.x),
        i64::from(r.y),
        u64::from(r.width),
        u64::from(r.height),
    )
}

fn device_rect(r: &Rect) -> protocol::Rect {
    protocol::Rect {
        x: r.x as u32,
        y: r.y as u32,
        width: r.width as u32,
        height: r.height as u32,
    }
}

impl Display {
    /// The changes that `commit` makes, checked against the outputs, with
    /// the sync objects and values to wait for.
    #[allow(clippy::type_complexity)]
    fn check(&self, commit: &Commit) -> Result<(Vec<Change>, Vec<(Rc<OwnedHandle>, u64)>), Status> {
        if commit.flags.contains(CommitFlags::ASYNC) {
            return Err(Status::InvalidArgs);
        }
        let outputs = self.outputs.borrow();
        let crtcs = self.crtcs.borrow();
        let buffers = self.buffers.borrow();
        let syncs = self.syncs.borrow();
        let mut changes: Vec<Change> = Vec::new();
        let mut waits = Vec::new();
        for crtc in &commit.crtcs {
            let index = (crtc.crtc as usize).wrapping_sub(1);
            let output = outputs.get(index).ok_or(Status::InvalidArgs)?;
            let extras = crtc.vrr
                || crtc.gamma.is_some()
                || crtc.hdr.is_some()
                || crtc.colorspace != Colorspace::Default;
            let reported = output.modes.iter().any(|m| {
                m.width == crtc.mode.width
                    && m.height == crtc.mode.height
                    && m.refresh_millihertz == crtc.mode.refresh_millihertz
            });
            let connector = index as u32 + 1;
            if extras || (crtc.active && (crtc.connector != connector || !reported)) {
                return Err(Status::InvalidArgs);
            }
            let at = change_at(&mut changes, &crtcs, index);
            changes[at].active = crtc.active;
            changes[at].mode = crtc.active.then_some((crtc.mode.width, crtc.mode.height));
        }
        for plane in &commit.planes {
            let index = ((plane.plane as usize).wrapping_sub(1)) / 2;
            if index >= outputs.len() {
                return Err(Status::InvalidArgs);
            }
            let is_cursor = plane.plane == cursor_plane(index);
            let at = change_at(&mut changes, &crtcs, index);
            if plane.crtc == 0 {
                match is_cursor {
                    true => changes[at].cursor = None,
                    false => changes[at].shown = None,
                }
                continue;
            }
            let buffer = buffers
                .get(&plane.buffer)
                .filter(|b| !b.destroyed)
                .ok_or(Status::InvalidArgs)?;
            let (src, dst) = (rect(&plane.src), rect(&plane.dst));
            let (width, height) = (buffer.resource.width, buffer.resource.height);
            let whole = Rect::new(0, 0, u64::from(width), u64::from(height));
            let same_size = src.width == dst.width && src.height == dst.height;
            if plane.crtc as usize != index + 1 || !whole.contains(&src) || !same_size {
                return Err(Status::InvalidArgs);
            }
            if is_cursor {
                let cursor = buffer.format == ARGB8888
                    && width == CURSOR
                    && height == CURSOR
                    && src == whole;
                if !cursor {
                    return Err(Status::InvalidArgs);
                }
                // The cursor's position is the image's top-left corner,
                // kept on the screen.
                changes[at].cursor = Some(Cursor {
                    buffer: plane.buffer,
                    x: plane.dst.x.max(0) as u32,
                    y: plane.dst.y.max(0) as u32,
                });
            } else {
                let mode = changes[at].mode.ok_or(Status::InvalidArgs)?;
                let screen = Rect::new(0, 0, u64::from(mode.0), u64::from(mode.1));
                if dst != screen || src.width == 0 {
                    return Err(Status::InvalidArgs);
                }
                changes[at].shown = Some(Shown {
                    buffer: plane.buffer,
                    src,
                });
                changes[at].damage = plane.damage.iter().map(rect).collect();
            }
            if plane.acquire_sync != 0 {
                let timeline = syncs.get(&plane.acquire_sync).ok_or(Status::InvalidArgs)?;
                waits.push((timeline.clone(), plane.acquire_value));
            }
        }
        for c in &changes {
            if !c.active && (c.shown.is_some() || c.cursor.is_some()) {
                return Err(Status::InvalidArgs);
            }
            if crtcs[c.index].pending {
                return Err(Status::ShouldWait);
            }
        }
        Ok((changes, waits))
    }

    pub(super) fn commit(
        self: &Rc<Self>,
        commit: Commit,
        owner: &Rc<DisplayAsyncServer>,
    ) -> Result<u64, Status> {
        let (changes, waits) = self.check(&commit)?;
        let id = self.next_commit.get();
        self.next_commit.set(id + 1);
        if commit.flags.contains(CommitFlags::TEST_ONLY) {
            return Ok(id);
        }
        let mut showing = Vec::new();
        for c in &changes {
            self.crtcs.borrow_mut()[c.index].pending = true;
            showing.extend(c.shown.map(|s| s.buffer));
            showing.extend(c.cursor.map(|s| s.buffer));
        }
        self.showing.borrow_mut().insert(id, showing);
        let presented = self
            .clone()
            .present(id, changes, waits, Rc::downgrade(owner));
        self.spawner.spawn(presented);
        Ok(id)
    }

    /// Presents commit `id` once its sync objects reached their values.
    async fn present(
        self: Rc<Self>,
        id: u64,
        changes: Vec<Change>,
        waits: Vec<(Rc<OwnedHandle>, u64)>,
        owner: Weak<DisplayAsyncServer>,
    ) {
        for (timeline, value) in waits {
            let _ = self.reactor.timeline(timeline.raw(), value).await;
        }
        let epoch = self.epoch.get();
        let last = changes.len().saturating_sub(1);
        for (i, change) in changes.iter().enumerate() {
            // The owner closed its connection, and the scanouts are off.
            if self.epoch.get() != epoch {
                break;
            }
            let fence = (i == last).then(|| self.gpu.fence());
            if let Err(status) = self.apply(change, fence).await {
                mikros_rt::log!(Error, "virtiogpu: presenting commit {id}: {status:?}");
            }
            if self.epoch.get() != epoch {
                break;
            }
            let mut crtcs = self.crtcs.borrow_mut();
            let crtc = &mut crtcs[change.index];
            crtc.active = change.active;
            crtc.mode = change.mode;
            crtc.shown = change.shown;
            crtc.cursor = change.cursor;
            crtc.pending = false;
        }
        self.showing.borrow_mut().remove(&id);
        self.release_unused();
        if let Some(owner) = owner.upgrade() {
            let _ = owner.server().send_on_commit_complete(id, now());
        }
    }

    /// Shows `change` on its scanout: the primary plane's buffer, copied
    /// to the host where damaged and flushed, the last command fenced
    /// with `fence`, and the cursor.
    async fn apply(&self, change: &Change, fence: Option<u64>) -> Result<(), Status> {
        let scanout = change.index as u32;
        let before = self.crtcs.borrow()[change.index];
        match change.shown {
            None if before.shown.is_some() || before.active != change.active => {
                let off = protocol::set_scanout(scanout, 0, protocol::Rect::default());
                self.gpu.ok(&off).await?;
            }
            None => {}
            Some(shown) => {
                let Some(resource) = self.prepare(&shown, &change.damage, &before) else {
                    return Err(Status::BadState);
                };
                if before.shown != Some(shown) || before.mode != change.mode {
                    let set = protocol::set_scanout(scanout, resource.id, device_rect(&shown.src));
                    self.gpu.ok(&set).await?;
                }
                let damage = match before.shown == Some(shown) {
                    true => copied(&shown.src, &change.damage),
                    false => vec![shown.src],
                };
                for r in &damage {
                    let offset = r.y as u64 * u64::from(resource.stride) + r.x as u64 * 4;
                    let transfer =
                        protocol::transfer_to_host_2d(resource.id, device_rect(r), offset);
                    self.gpu.ok(&transfer).await?;
                }
                let flush = protocol::resource_flush(resource.id, device_rect(&shown.src), fence);
                self.gpu.ok(&flush).await?;
            }
        }
        self.apply_cursor(scanout, before.cursor, change.cursor)
            .await
    }

    /// The resource of `shown`, with the parts of an imported buffer that
    /// the commit copies brought over from the client's memory.
    fn prepare(&self, shown: &Shown, damage: &[Rect], before: &Crtc) -> Option<Rc<Resource>> {
        let buffers = self.buffers.borrow();
        let buffer = buffers.get(&shown.buffer)?;
        if let Source::Imported {
            mapping,
            start,
            pitch,
            ..
        } = &buffer.source
        {
            let parts = match before.shown == Some(*shown) {
                true => copied(&shown.src, damage),
                false => vec![shown.src],
            };
            copy_in(mapping, *start, *pitch, &buffer.resource, &parts);
        }
        Some(buffer.resource.clone())
    }

    /// Shows the cursor `now` in place of `before` on `scanout`: a new
    /// image with `UPDATE_CURSOR`, after copying it to the host, and a
    /// move alone with `MOVE_CURSOR`.
    async fn apply_cursor(
        &self,
        scanout: u32,
        before: Option<Cursor>,
        now: Option<Cursor>,
    ) -> Result<(), Status> {
        match now {
            None if before.is_some() => {
                self.gpu
                    .cursor(&protocol::update_cursor(scanout, 0, 0, 0, (0, 0)))
                    .await
            }
            None => Ok(()),
            Some(c) if before.is_some_and(|b| b.buffer == c.buffer) => {
                if before != Some(c) {
                    self.gpu
                        .cursor(&protocol::move_cursor(scanout, c.x, c.y))
                        .await?;
                }
                Ok(())
            }
            Some(c) => {
                let resource = {
                    let buffers = self.buffers.borrow();
                    let buffer = buffers.get(&c.buffer).ok_or(Status::BadState)?;
                    let whole = Rect::new(0, 0, u64::from(CURSOR), u64::from(CURSOR));
                    if let Source::Imported {
                        mapping,
                        start,
                        pitch,
                        ..
                    } = &buffer.source
                    {
                        copy_in(mapping, *start, *pitch, &buffer.resource, &[whole]);
                    }
                    buffer.resource.clone()
                };
                let all = protocol::Rect {
                    x: 0,
                    y: 0,
                    width: CURSOR,
                    height: CURSOR,
                };
                self.gpu
                    .ok(&protocol::transfer_to_host_2d(resource.id, all, 0))
                    .await?;
                self.gpu
                    .cursor(&protocol::update_cursor(
                        scanout,
                        c.x,
                        c.y,
                        resource.id,
                        (0, 0),
                    ))
                    .await
            }
        }
    }
}

/// Where in `changes` the change of CRTC `index` is, added with what
/// `crtcs` show now when there is none yet.
fn change_at(changes: &mut Vec<Change>, crtcs: &[Crtc], index: usize) -> usize {
    if let Some(at) = changes.iter().position(|c| c.index == index) {
        return at;
    }
    let now = crtcs[index];
    changes.push(Change {
        index,
        active: now.active,
        mode: now.mode,
        shown: now.shown,
        cursor: now.cursor,
        damage: Vec::new(),
    });
    changes.len() - 1
}

/// Copies `parts` of the client's image at `start` in `mapping`, `pitch`
/// bytes per line, into `resource`.
fn copy_in(mapping: &Mapping, start: u64, pitch: u32, resource: &Resource, parts: &[Rect]) {
    let mut line = Vec::new();
    for r in parts {
        line.resize(r.width as usize * 4, 0);
        for row in 0..r.height {
            let y = r.y as u64 + row;
            let x = r.x as u64 * 4;
            mapping.read(start + y * u64::from(pitch) + x, &mut line);
            resource
                .mapping
                .write(y * u64::from(resource.stride) + x, &line);
        }
    }
}
