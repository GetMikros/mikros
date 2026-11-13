//! The text console: the full-screen terminal that runs when no desktop
//! does. The session manager starts it with the `Seat` channel, through
//! which it gets the displays and input devices, and the `TextLogin`
//! channel, through which it runs setup and logs users in. It draws a
//! screen on the first display with the monospaced font, turns key
//! presses into the terminal's input with the system keyboard layout and
//! key repeat, and shows either its own login prompt or the terminal of
//! setup or of the user's session. It runs six such text consoles, each
//! with its own login prompt, setup or session, and shows one at a time:
//! Ctrl, Alt and F1 to F6 show the others, which keep their contents
//! meanwhile. While no session runs on the console shown, the system log
//! fills the rows above the login prompt or setup's terminal, and while
//! Ctrl, Alt and F12 are held, the whole screen. In the diagnostic build,
//! the system overview fills the screen while Ctrl, Alt and F11 are
//! held.

#![feature(mikros_handles)]

mod font;
mod keyboard;
mod login;
mod logpane;
mod notify;
#[cfg(mikros_diag)]
mod overview;
mod palette;
mod process;
mod render;
mod term;
mod vt;
mod xkb;

use keyboard::{Keyboard, Pressed};
use logpane::LogPane;
use mikros_evdev::codes::{EV_KEY, KEY_A};
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::gpu::DisplayAsyncClient;
use mikros_rt::interfaces::mikros::input::{DeviceAsyncClient, DeviceEvent};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::session::{
    SeatAddDisplayRequest, SeatAddInputRequest, SeatAsyncServer, SeatRequest, TextLoginAsyncClient,
};
use mikros_rt::sys;
use mikros_vt::screen::{Modes, Screen};
use notify::Notify;
use render::Renderer;
use std::cell::{Cell, RefCell};
use std::os::mikros::startup;
use std::rc::Rc;
use vt::{View, Vt};

/// The `USER` role, and the `id`s of the `TextLogin` and `Seat` channels.
const USER: u32 = 16;
const TEXT_LOGIN_ID: u32 = 3;
const SEAT_ID: u32 = 7;
const SAME_RIGHTS: u32 = 1 << 31;
/// The keyboard layout until `/etc/keyboard.yaml` names one.
const LAYOUT: &str = "us";
/// The rows of the login prompt and of setup's terminal, below the log.
const PROMPT_ROWS: usize = 8;
/// The number of text consoles.
const CONSOLES: u8 = 6;

/// How the screen's rows are shared: the log's rows at the top, and
/// those of the view below them.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Layout {
    log: usize,
    view: usize,
}

pub struct Console {
    reactor: Reactor,
    spawner: Spawner,
    directory: DirectoryAsyncClient,
    renderer: RefCell<Option<Renderer>>,
    keyboard: RefCell<Option<Keyboard>>,
    /// The text consoles, 1 to 6, and the index of the one shown.
    vts: Vec<Rc<Vt>>,
    shown: Cell<usize>,
    /// Whether the session manager closed `TextLogin`.
    closed: Cell<bool>,
    log: LogPane,
    /// Whether Ctrl, Alt and F12 are held.
    log_held: Cell<bool>,
    /// In the diagnostic build, the system overview.
    #[cfg(mikros_diag)]
    overview: overview::Overview,
    /// The size of the grid of cells, once a display came.
    grid: Cell<(usize, usize)>,
    /// Woken when the screen changed, and when the held key changed.
    changed: Rc<Notify>,
    keys: Notify,
}

pub(crate) fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// A copy of `raw`, which the console then owns.
fn duplicate(raw: sys::Handle) -> Option<OwnedHandle> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(raw, SAME_RIGHTS, &mut out) };
    // SAFETY: the kernel just created the handle for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
}

/// The startup handle `id` of the `USER` role, as mikros-rt's handle.
fn take(id: u32) -> Option<OwnedHandle> {
    let handle = startup::take_handle(USER, id)?;
    // SAFETY: the handle is ours, and is handed over once.
    Some(unsafe { OwnedHandle::from_raw(handle.into_raw_handle()) })
}

impl Console {
    /// The screen shown now, for `f`.
    fn with_screen<T>(&self, f: impl FnOnce(&mut Screen) -> T) -> T {
        self.vt().with_screen(f)
    }

    /// The text console shown.
    fn vt(&self) -> &Rc<Vt> {
        &self.vts[self.shown.get()]
    }

    /// Shows text console `number`, 1 to 6, as it was left.
    fn switch_to(&self, number: u8) {
        let Some(index) = usize::from(number)
            .checked_sub(1)
            .filter(|&i| i < self.vts.len())
        else {
            return;
        };
        if self.shown.replace(index) != index {
            self.with_screen(|s| s.touch());
            self.changed.notify();
        }
    }

    fn modes(&self) -> Modes {
        self.with_screen(|s| s.modes)
    }

    /// The rows of the log and of the view: the whole screen is the log
    /// while Ctrl, Alt and F12 are held, and the session's terminal
    /// while a session runs; otherwise the log fills the rows above the
    /// bottom `PROMPT_ROWS`.
    fn layout(&self) -> Layout {
        let rows = self.grid.get().1;
        if self.log_held.get() || self.overview_shown() || self.vt().log_only.get() {
            return Layout { log: rows, view: 0 };
        }
        match self.vt().session.get() {
            true => Layout { log: 0, view: rows },
            false => {
                let view = PROMPT_ROWS.min(rows);
                Layout {
                    log: rows - view,
                    view,
                }
            }
        }
    }

    /// Whether the system overview of the diagnostic build fills the
    /// screen, in the place of the log.
    fn overview_shown(&self) -> bool {
        #[cfg(mikros_diag)]
        return self.overview.shown();
        #[cfg(not(mikros_diag))]
        false
    }

    /// The screen of the log's rows, `cols` by `rows`, when it changed
    /// or `whole` is set: the system overview's while it shows.
    fn top_screen(&self, cols: usize, rows: usize, whole: bool) -> Option<Screen> {
        #[cfg(mikros_diag)]
        if self.overview.shown() {
            return (self.overview.take_stale() || whole).then(|| self.overview.screen(cols, rows));
        }
        (rows > 0 && (self.log.take_stale() || whole)).then(|| self.log.screen(cols, rows))
    }

    /// The columns and rows of the login prompt and of setup's terminal.
    fn prompt_size(&self) -> (usize, usize) {
        let (cols, rows) = self.grid.get();
        (cols, PROMPT_ROWS.min(rows))
    }

    /// Makes `view` what text console `vt` shows, whole, which is the
    /// terminal of a session when `session` is set.
    fn show(&self, vt: &Vt, view: View, session: bool) {
        *vt.view.borrow_mut() = view;
        vt.session.set(session);
        vt.with_screen(|s| s.touch());
        self.changed.notify();
    }

    /// Takes in `bytes` typed: the terminal of the console shown gets
    /// them, or its login prompt.
    fn input(self: &Rc<Self>, bytes: Vec<u8>) {
        let vt = self.vt().clone();
        match vt.term() {
            Some(term) => {
                let reactor = self.reactor.clone();
                self.spawner
                    .spawn(async move { term.type_in(&reactor, &bytes).await });
            }
            None => {
                vt.typed.borrow_mut().extend(bytes);
                vt.typed_in.notify();
            }
        }
    }

    /// Redraws the rows that changed, for as long as the console runs:
    /// those of the log when lines came, and those of the view.
    async fn redraw(self: Rc<Self>) {
        // The view's row the cursor was drawn on, which is drawn again
        // when the cursor has left it, so that it shows the cursor no
        // more.
        let mut drawn_cursor: Option<usize> = None;
        // The layout and grid drawn, which, when they change, make the
        // whole screen be drawn again.
        let mut drawn: Option<(Layout, (usize, usize))> = None;
        loop {
            self.changed.changed().await;
            let (layout, grid) = (self.layout(), self.grid.get());
            let whole = drawn != Some((layout, grid));
            drawn = Some((layout, grid));
            let log = self.top_screen(grid.0, layout.log, whole);
            let log_rows: Vec<usize> = match log {
                Some(_) => (0..layout.log).collect(),
                None => Vec::new(),
            };
            let (mut rows, cursor) = self.with_screen(|s| {
                if whole {
                    s.touch();
                }
                (s.take_dirty(), s.cursor())
            });
            let cursor_row = cursor.map(|(row, _)| row);
            rows.extend(drawn_cursor.into_iter().chain(cursor_row));
            rows.retain(|&row| row < layout.view);
            rows.sort_unstable();
            rows.dedup();
            drawn_cursor = cursor_row;
            let damaged: Vec<usize> = log_rows
                .iter()
                .copied()
                .chain(rows.iter().map(|row| row + layout.log))
                .collect();
            let mut renderer = self.renderer.borrow_mut().take();
            if let Some(r) = renderer.as_mut() {
                if let Some(log) = &log {
                    r.draw(log, &log_rows, 0);
                }
                self.with_screen(|s| r.draw(s, &rows, layout.log));
                let _ = r.present(&damaged).await;
            }
            if self.renderer.borrow().is_none() {
                *self.renderer.borrow_mut() = renderer;
            }
        }
    }

    /// Repeats the held key, for as long as the console runs.
    async fn repeat(self: Rc<Self>) {
        loop {
            let next = self
                .keyboard
                .borrow()
                .as_ref()
                .and_then(Keyboard::next_repeat);
            match next {
                Some(at) => {
                    let sleep = self.reactor.sleep_until(at);
                    first(self.keys.changed(), sleep).await;
                }
                None => self.keys.changed().await,
            }
            let modes = self.modes();
            let bytes = self
                .keyboard
                .borrow_mut()
                .as_mut()
                .and_then(|k| k.repeat(now(), &modes));
            if let Some(bytes) = bytes {
                self.input(bytes);
            }
        }
    }

    async fn add_display(
        self: Rc<Self>,
        display: ClientEnd<mikros_rt::interfaces::mikros::gpu::Display>,
    ) {
        if self.renderer.borrow().is_some() {
            return;
        }
        let display = DisplayAsyncClient::new(display, self.reactor.clone());
        let renderer = match Renderer::new(display, process::current()).await {
            Ok(renderer) => renderer,
            Err(why) => {
                eprintln!("console: the display cannot be used: {why}");
                return;
            }
        };
        let (cols, rows) = renderer.grid();
        self.grid.set((cols, rows));
        *self.renderer.borrow_mut() = Some(renderer);
        // Every console gets the grid: a session's terminal all its rows,
        // the login prompt and setup's terminal those below the log.
        for vt in self.vts.clone() {
            let vt_rows = match vt.session.get() {
                true => rows,
                false => PROMPT_ROWS.min(rows),
            };
            match vt.term() {
                Some(term) => term.resize(cols, vt_rows).await,
                None => vt.with_screen(|s| s.resize(cols, vt_rows)),
            }
        }
        self.with_screen(|s| s.touch());
        self.changed.notify();
    }

    /// Reads key events of the input device `device` while it is open.
    async fn add_input(
        self: Rc<Self>,
        instance: String,
        device: ClientEnd<mikros_rt::interfaces::mikros::input::Device>,
        accepted: bool,
    ) {
        let device = DeviceAsyncClient::new(device, self.reactor.clone());
        let Ok(info) = device.get_info().await else {
            return;
        };
        let keyboard = info.info.keys[usize::from(KEY_A / 8)] & (1 << (KEY_A % 8)) != 0;
        if !keyboard {
            return;
        }
        // Key events of a keyboard that is not accepted are not acted on.
        if !accepted {
            eprintln!(
                "console: the keyboard {} ({instance}) is not accepted",
                info.info.name
            );
            return;
        }
        if self.keyboard.borrow().is_none() {
            let names = xkb::Names {
                layout: LAYOUT.into(),
                ..Default::default()
            };
            match xkb::Keymap::new(&names) {
                Some(keymap) => *self.keyboard.borrow_mut() = Some(Keyboard::new(keymap)),
                None => {
                    eprintln!("console: the keymap cannot be compiled");
                    return;
                }
            }
        }
        while let Ok(DeviceEvent::OnEvents(batch)) = device.next_event().await {
            for event in batch
                .events
                .iter()
                .filter(|e| e.r#type == EV_KEY && e.value != 2)
            {
                let modes = self.modes();
                let pressed = self
                    .keyboard
                    .borrow_mut()
                    .as_mut()
                    .map(|k| k.key(event.code, event.value, now(), &modes));
                self.keys.notify();
                let held = self
                    .keyboard
                    .borrow()
                    .as_ref()
                    .is_some_and(Keyboard::log_view);
                if self.log_held.replace(held) != held {
                    self.changed.notify();
                }
                #[cfg(mikros_diag)]
                {
                    let shown = self
                        .keyboard
                        .borrow()
                        .as_ref()
                        .is_some_and(Keyboard::overview);
                    if self.overview.set_shown(shown) {
                        self.changed.notify();
                    }
                }
                match pressed {
                    Some(Pressed::Bytes(bytes)) => self.input(bytes),
                    Some(Pressed::Switch(number)) => self.switch_to(number),
                    Some(Pressed::LogView) | Some(Pressed::Nothing) | None => {}
                    #[cfg(mikros_diag)]
                    Some(Pressed::Overview) => {}
                }
            }
        }
    }

    async fn serve_seat(self: Rc<Self>, seat: OwnedHandle) {
        let server = SeatAsyncServer::new(ServerEnd::new(seat), self.reactor.clone());
        while let Ok(request) = server.next().await {
            match request {
                SeatRequest::AddDisplay(SeatAddDisplayRequest { display, .. }) => {
                    self.spawner.spawn(self.clone().add_display(display));
                }
                SeatRequest::AddInput(SeatAddInputRequest {
                    instance,
                    device,
                    accepted,
                }) => {
                    self.spawner
                        .spawn(self.clone().add_input(instance, device, accepted));
                }
                // No desktop is installed, so screens 1 to 6 are the
                // consoles 1 to 6.
                SeatRequest::SetScreen(screen) => self.switch_to(screen.screen),
            }
        }
    }
}

/// Completes when the first of `a` and `b` completes.
pub(crate) async fn first<A: std::future::Future, B: std::future::Future>(a: A, b: B) {
    let mut a = std::pin::pin!(a);
    let mut b = std::pin::pin!(b);
    std::future::poll_fn(|cx| {
        if a.as_mut().poll(cx).is_ready() || b.as_mut().poll(cx).is_ready() {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}

fn main() {
    let (Some(text_login), Some(seat)) = (take(TEXT_LOGIN_ID), take(SEAT_ID)) else {
        eprintln!("console: the TextLogin and Seat channels are missing");
        std::process::exit(1);
    };
    let Some(directory) = duplicate(std::os::mikros::directory().as_raw_handle()) else {
        std::process::exit(1);
    };
    let Ok(mut executor) = Executor::new() else {
        std::process::exit(1);
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let console = Rc::new(Console {
        reactor: reactor.clone(),
        spawner: spawner.clone(),
        directory: DirectoryAsyncClient::new(ClientEnd::new(directory), reactor.clone()),
        renderer: RefCell::new(None),
        keyboard: RefCell::new(None),
        vts: (1..=CONSOLES)
            .map(|n| Vt::new(n, render::MIN_COLS, PROMPT_ROWS))
            .collect(),
        shown: Cell::new(0),
        closed: Cell::new(false),
        log: LogPane::new(),
        log_held: Cell::new(false),
        #[cfg(mikros_diag)]
        overview: overview::Overview::default(),
        grid: Cell::new((render::MIN_COLS, render::MIN_ROWS)),
        changed: Rc::new(Notify::default()),
        keys: Notify::default(),
    });
    let text_login = Rc::new(TextLoginAsyncClient::new(
        ClientEnd::new(text_login),
        reactor.clone(),
    ));
    spawner.spawn(console.clone().serve_seat(seat));
    spawner.spawn(console.clone().redraw());
    spawner.spawn(logpane::follow(console.clone()));
    #[cfg(mikros_diag)]
    spawner.spawn(overview::follow(console.clone()));
    spawner.spawn(console.clone().repeat());
    for vt in console.vts.clone() {
        spawner.spawn(login::run(console.clone(), vt, text_login.clone()));
    }
    executor.block_on(login::events(console, text_login));
}
