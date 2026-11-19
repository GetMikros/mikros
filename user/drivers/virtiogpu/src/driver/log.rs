//! The screen that shows the log while no client owns the display: a
//! resource in the preferred mode of the first connected scanout, which
//! the gpudev library writes rows into. A task of the driver's own
//! copies the rows written since its last turn to the host and flushes
//! them, setting the scanout to the resource first whenever another
//! buffer was shown meanwhile.

use crate::driver::gpu::{Gpu, Resource};
use crate::protocol;
use alloc::rc::Rc;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_gpudev::Screen;

pub struct LogScreen {
    gpu: Rc<Gpu>,
    /// The resource and the scanout it is for.
    screen: RefCell<Option<(Resource, u32)>>,
    /// The rows written and not yet flushed, as the first and the one
    /// after the last.
    dirty: Cell<Option<(u32, u32)>>,
    /// Whether the scanout shows the resource, and whether a client owns
    /// the display, which leaves the scanouts to it.
    shown: Cell<bool>,
    owned: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}

impl LogScreen {
    pub fn new(gpu: Rc<Gpu>) -> LogScreen {
        LogScreen {
            gpu,
            screen: RefCell::new(None),
            dirty: Cell::new(None),
            shown: Cell::new(false),
            owned: Cell::new(false),
            waker: RefCell::new(None),
        }
    }

    /// Shows the log on `scanout` in a resource of `width` by `height`,
    /// in place of the one before, which is released.
    pub async fn place(&self, scanout: u32, width: u32, height: u32) {
        let same = self
            .screen
            .borrow()
            .as_ref()
            .is_some_and(|(r, s)| *s == scanout && r.width == width && r.height == height);
        if same {
            return;
        }
        let created = self
            .gpu
            .create(width, height, protocol::FORMAT_B8G8R8X8)
            .await;
        let old = match created {
            Ok(resource) => self.screen.replace(Some((resource, scanout))),
            Err(status) => {
                mikros_rt::log!(Error, "virtiogpu: no screen for the log: {status:?}");
                self.screen.replace(None)
            }
        };
        self.shown.set(false);
        if let Some((old, _)) = old {
            self.gpu.release(old).await;
        }
    }

    /// The scanout that shows the log.
    pub fn scanout(&self) -> Option<u32> {
        self.screen.borrow().as_ref().map(|(_, s)| *s)
    }

    /// Leaves the scanouts to the client that now owns the display.
    pub fn owned(&self) {
        self.owned.set(true);
        self.dirty.set(None);
    }

    /// Has the next flush set the scanout to the log's resource again,
    /// once the client that showed its own buffers closed its connection.
    pub fn released(&self) {
        self.owned.set(false);
        self.shown.set(false);
    }

    /// Copies the rows written to the host and flushes them, for as long
    /// as the driver runs.
    pub async fn flush(self: Rc<Self>) {
        loop {
            let (first, end) = poll_fn(|cx| match self.dirty.take() {
                Some(rows) => Poll::Ready(rows),
                None => {
                    *self.waker.borrow_mut() = Some(cx.waker().clone());
                    Poll::Pending
                }
            })
            .await;
            if self.owned.get() {
                continue;
            }
            let (id, scanout, width, height, stride) = {
                let screen = self.screen.borrow();
                let Some((r, scanout)) = screen.as_ref() else {
                    continue;
                };
                (r.id, *scanout, r.width, r.height, r.stride)
            };
            if !self.shown.replace(true) {
                let whole = protocol::Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                };
                let _ = self
                    .gpu
                    .ok(&protocol::set_scanout(scanout, id, whole))
                    .await;
            }
            let rows = protocol::Rect {
                x: 0,
                y: first,
                width,
                height: end.min(height).saturating_sub(first),
            };
            let offset = u64::from(first) * u64::from(stride);
            if self.owned.get() {
                continue;
            }
            let _ = self
                .gpu
                .ok(&protocol::transfer_to_host_2d(id, rows, offset))
                .await;
            let _ = self.gpu.ok(&protocol::resource_flush(id, rows, None)).await;
        }
    }
}

impl Screen for LogScreen {
    fn size(&self) -> (u32, u32) {
        self.screen
            .borrow()
            .as_ref()
            .map_or((0, 0), |(r, _)| (r.width, r.height))
    }

    fn write_row(&self, y: u32, pixels: &mut dyn Iterator<Item = u32>) {
        if self.owned.get() {
            return;
        }
        let screen = self.screen.borrow();
        let Some((r, _)) = screen.as_ref() else {
            return;
        };
        if y >= r.height {
            return;
        }
        let line = u64::from(y) * u64::from(r.stride);
        for (x, pixel) in pixels.take(r.width as usize).enumerate() {
            r.mapping.write32(line + x as u64 * 4, pixel);
        }
        let rows = match self.dirty.get() {
            Some((first, end)) => (first.min(y), end.max(y + 1)),
            None => (y, y + 1),
        };
        self.dirty.set(Some(rows));
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}
