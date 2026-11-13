//! The system overview of the diagnostic build: the text of the
//! diagnostic service's `Overview.Snapshot`, on a screen of its own while
//! Ctrl, Alt and F11 are held, taken again every second.

use crate::emulator::cell::columns;
use crate::emulator::screen::Screen;
use crate::program::Console;
use crate::program::notify::Notify;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::diag::{Overview as Protocol, OverviewAsyncClient};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const OVERVIEW: &str = "mikros.diag.Overview";
/// How often the text is taken again while it shows.
const REFRESH: i64 = 1_000_000_000;

#[derive(Default)]
pub struct Overview {
    shown: Cell<bool>,
    text: RefCell<String>,
    /// Whether the text changed since the last screen was made.
    stale: Cell<bool>,
    /// Woken when the overview is shown or hidden.
    wake: Notify,
}

impl Overview {
    pub fn shown(&self) -> bool {
        self.shown.get()
    }

    /// Shows or hides the overview, and returns whether that changed.
    pub fn set_shown(&self, shown: bool) -> bool {
        if self.shown.replace(shown) == shown {
            return false;
        }
        self.stale.set(true);
        self.wake.notify();
        true
    }

    /// Whether the text changed since the last screen was made; forgets
    /// it.
    pub fn take_stale(&self) -> bool {
        self.stale.replace(false)
    }

    /// A screen of `cols` by `rows` with the first lines of the text that
    /// fit, a line to a row, each cut to the width, and no cursor.
    pub fn screen(&self, cols: usize, rows: usize) -> Screen {
        let mut screen = Screen::new(cols, rows);
        screen.write(b"\x1b[?25l");
        let text = self.text.borrow();
        for (row, line) in text.lines().take(rows).enumerate() {
            // The service's text has no control characters, so that no
            // line is an escape sequence; any that come are dropped.
            let line: String = line.chars().filter(|c| !c.is_control()).collect();
            screen.write(format!("\x1b[{};1H", row + 1).as_bytes());
            screen.write(mikros_logview::fit(&line, cols, columns).as_bytes());
        }
        screen.touch();
        screen
    }

    fn set_text(&self, text: String) {
        *self.text.borrow_mut() = text;
        self.stale.set(true);
    }
}

/// Takes the overview's text whenever it shows and then every second, for
/// as long as the console runs.
pub async fn follow(console: Rc<Console>) {
    let overview = &console.overview;
    let mut client: Option<OverviewAsyncClient> = None;
    loop {
        while !overview.shown() {
            overview.wake.changed().await;
        }
        if client.is_none() {
            client = connect(&console).await;
        }
        let text = match &client {
            None => "The diagnostic service cannot be reached.".to_owned(),
            Some(c) => match c.snapshot().await {
                Ok(Ok(r)) => r.text,
                Ok(Err(status)) => {
                    format!("The diagnostic service refused the overview: {status:?}")
                }
                Err(_) => {
                    client = None;
                    "The diagnostic service closed the connection.".to_owned()
                }
            },
        };
        overview.set_text(text);
        console.changed.notify();
        let sleep = console
            .reactor
            .sleep_until(crate::program::now().saturating_add(REFRESH));
        crate::program::first(sleep, overview.wake.changed()).await;
    }
}

async fn connect(console: &Console) -> Option<OverviewAsyncClient> {
    let (client, server) = endpoints::<Protocol>().ok()?;
    let connected = console
        .directory
        .connect(OVERVIEW.into(), None, server.into_handle())
        .await;
    matches!(connected, Ok(Ok(_)))
        .then(|| OverviewAsyncClient::new(client, console.reactor.clone()))
}
