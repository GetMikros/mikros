//! The system log on the console: the records since boot, read through
//! the log service's `Reader` and then followed, kept as lines, of which
//! the console shows the most recent in a screen of their own.

use crate::Console;
use mikros_logview::Tail;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::log::{Reader, ReaderAsyncClient, ReaderEvent, Record};
use mikros_vt::cell::columns;
use mikros_vt::screen::Screen;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const READER: &str = "mikros.log.Reader";
/// The most records one `Read` gives.
const BATCH: u32 = 32;
/// The most lines kept, more than any screen has rows.
const KEPT: usize = 512;

pub struct LogPane {
    tail: RefCell<Tail>,
    /// Whether lines came since the last screen was made.
    stale: Cell<bool>,
}

impl LogPane {
    pub fn new() -> LogPane {
        LogPane {
            tail: RefCell::new(Tail::new(KEPT)),
            stale: Cell::new(false),
        }
    }

    fn take(&self, records: &[Record]) {
        let mut tail = self.tail.borrow_mut();
        for r in records {
            let line = mikros_logview::line(r.timestamp, &r.app_id, &r.message);
            if tail.push(r.sequence, line) {
                self.stale.set(true);
            }
        }
    }

    /// Whether lines came since the last screen was made; forgets it.
    pub fn take_stale(&self) -> bool {
        self.stale.replace(false)
    }

    /// A screen of `cols` by `rows` with the most recent lines that fit,
    /// a line to a row from the top, each cut to the width, and no
    /// cursor. The lines have no control characters, so that none of
    /// them is an escape sequence.
    pub fn screen(&self, cols: usize, rows: usize) -> Screen {
        let mut screen = Screen::new(cols, rows);
        screen.write(b"\x1b[?25l");
        for (row, (_, line)) in self.tail.borrow().last(rows).enumerate() {
            screen.write(format!("\x1b[{};1H", row + 1).as_bytes());
            screen.write(mikros_logview::fit(line, cols, columns).as_bytes());
        }
        screen.touch();
        screen
    }
}

/// Reads the log into `console`'s pane, and wakes the console as lines
/// come, for as long as the log service serves the connection.
pub async fn follow(console: Rc<Console>) {
    let Ok((client, server)) = endpoints::<Reader>() else {
        return;
    };
    match console
        .directory
        .connect(READER.into(), None, server.into_handle())
        .await
    {
        Ok(Ok(_)) => {}
        _ => {
            eprintln!("console: the log cannot be read");
            return;
        }
    }
    let reader = ReaderAsyncClient::new(client, console.reactor.clone());
    // Following from the latest record before reading up to it leaves
    // none out; the tail takes a record read twice once.
    if !matches!(reader.follow().await, Ok(Ok(_))) {
        eprintln!("console: the log cannot be followed");
        return;
    }
    let pane = &console.log;
    loop {
        let after = pane.tail.borrow().latest();
        let Ok(read) = reader.read(after, BATCH).await else {
            return;
        };
        if read.records.is_empty() {
            break;
        }
        pane.take(&read.records);
    }
    console.changed.notify();
    while let Ok(ReaderEvent::OnRecords(event)) = reader.next_event().await {
        pane.take(&event.records);
        console.changed.notify();
    }
}
