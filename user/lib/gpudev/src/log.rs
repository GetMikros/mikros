//! The system log on the driver's screen while no client owns the
//! display: the most recent records that fit, read through the log
//! service's `Reader`, then each new one, a line each in the built-in
//! bitmap font, light on dark, scrolling up once the screen is full.
//! Nothing is drawn before the log service first answers. The records
//! go on being read while a client owns the display, so that the screen
//! shows the most recent ones again once it closes its connection.

use crate::{Backend, GpuDevice};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_display::text::{self, CELL_HEIGHT, CELL_WIDTH};
use mikros_logview::Tail;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::log::{Reader, ReaderAsyncClient, ReaderEvent, Record};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

const READER: &str = "mikros.log.Reader";
/// The most records one `Read` gives.
const BATCH: u32 = 32;
/// The most lines kept, more than any screen has rows.
const KEPT: usize = 512;
/// Light grey on black, as XRGB8888.
const TEXT: u32 = 0x00D0_D0D0;
const BACKGROUND: u32 = 0;

/// The screen that shows the log: the framebuffer, or a buffer that a
/// connector scans out.
pub trait Screen {
    /// Its width and height, in pixels.
    fn size(&self) -> (u32, u32);

    /// Writes `pixels`, XRGB8888 values, into line `y` from its left
    /// end.
    fn write_row(&self, y: u32, pixels: &mut dyn Iterator<Item = u32>);
}

/// What the screen shows of the log.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Drawn {
    size: (u32, u32),
    /// The sequence number of the record on the top row, if any.
    top: Option<u64>,
    /// The rows drawn, from the top: those that show records, or, while
    /// `whole`, those of the whole screen drawn so far.
    rows: usize,
    /// Set while the whole screen is being drawn again.
    whole: bool,
}

pub struct Log {
    tail: RefCell<Tail>,
    /// Whether the log service has answered.
    started: Cell<bool>,
    /// What the screen shows, or None when it does not show the log.
    drawn: Cell<Option<Drawn>>,
}

impl Log {
    pub fn new() -> Log {
        Log {
            tail: RefCell::new(Tail::new(KEPT)),
            started: Cell::new(false),
            drawn: Cell::new(None),
        }
    }

    fn take(&self, records: &[Record]) {
        let mut tail = self.tail.borrow_mut();
        for r in records {
            tail.push(
                r.sequence,
                mikros_logview::line(r.timestamp, &r.app_id, &r.message),
            );
        }
    }

    /// Forgets what the screen shows, after a client owned the display,
    /// so that the next drawing draws the whole screen.
    pub fn forget(&self) {
        self.drawn.set(None);
    }

    /// Draws on `screen` at most `budget` rows of what changed since the
    /// last drawing, once the log service has answered: the new rows, or
    /// the whole screen once it scrolled, changed size or showed
    /// something else. Returns whether the screen is up to date, so that
    /// a slow screen is drawn a part at a time with other work between.
    pub fn draw(&self, screen: &dyn Screen, budget: usize) -> bool {
        if !self.started.get() {
            return true;
        }
        let size = screen.size();
        let (width, height) = size;
        let cols = width as usize / CELL_WIDTH;
        let rows = height as usize / CELL_HEIGHT;
        let tail = self.tail.borrow();
        let lines: Vec<(u64, &str)> = tail.last(rows).collect();
        let top = lines.first().map(|l| l.0);
        let (from, whole) = match self.drawn.get() {
            Some(d) if d.size == size && d.top == top => (d.rows, d.whole),
            _ => (0, true),
        };
        let target = if whole { rows } else { lines.len() };
        let to = target.min(from.saturating_add(budget));
        for row in from..to {
            let line = lines
                .get(row)
                .map_or("", |l| mikros_logview::fit(l.1, cols, |_| 1));
            for y in 0..CELL_HEIGHT {
                let mut pixels = text::row(line, y, width as usize, TEXT, BACKGROUND);
                screen.write_row((row * CELL_HEIGHT + y) as u32, &mut pixels);
            }
        }
        let done = to >= target;
        if whole && done {
            // The pixels below the last row.
            for y in (rows * CELL_HEIGHT) as u32..height {
                screen.write_row(y, &mut core::iter::repeat_n(BACKGROUND, width as usize));
            }
        }
        self.drawn.set(Some(Drawn {
            size,
            top,
            rows: if done { lines.len() } else { to },
            whole: whole && !done,
        }));
        done
    }
}

/// Reads the log through `directory`'s `Reader` and draws it on
/// `device`'s screen, for as long as the log service serves the
/// connection.
pub async fn follow<B: Backend + 'static>(
    device: Rc<GpuDevice<B>>,
    directory: Rc<DirectoryAsyncClient>,
) {
    let Ok((client, server)) = endpoints::<Reader>() else {
        return;
    };
    // The connection waits while the log service does not run.
    match directory
        .connect(READER.into(), None, server.into_handle())
        .await
    {
        Ok(Ok(_)) => {}
        _ => {
            mikros_rt::log!(Error, "gpudev: the log cannot be read");
            return;
        }
    }
    let reader = ReaderAsyncClient::new(client, device.reactor.clone());
    // Following from the latest record before reading up to it leaves
    // none out; the tail takes a record read twice once.
    if !matches!(reader.follow().await, Ok(Ok(_))) {
        mikros_rt::log!(Error, "gpudev: the log cannot be followed");
        return;
    }
    let log = &device.log;
    loop {
        let after = log.tail.borrow().latest();
        let Ok(read) = reader.read(after, BATCH).await else {
            return;
        };
        if read.records.is_empty() {
            break;
        }
        log.take(&read.records);
    }
    log.started.set(true);
    device.draw_log();
    // Records only mark the screen out of date: drawing is the drawing
    // task's, which catches up with all of them at once.
    while let Ok(ReaderEvent::OnRecords(event)) = reader.next_event().await {
        log.take(&event.records);
        device.draw_log();
    }
}
