//! The text console: the full-screen terminal that runs when no desktop
//! does. The session manager starts it with the `Seat` channel, through
//! which it gets the displays and input devices, and the `TextLogin`
//! channel, through which it runs setup and logs users in. It draws a
//! screen on the first display with the monospaced font, turns key
//! presses into the terminal's input with the system keyboard layout and
//! key repeat, and shows either its own login prompt or the terminal of
//! setup or of the user's session. While no session runs, the system log
//! fills the rows above the login prompt or setup's terminal, and while
//! Ctrl, Alt and F12 are held, the whole screen.

#![feature(mikros_handles)]

mod font;
mod keyboard;
mod login;
mod logpane;
mod notify;
mod palette;
mod process;
mod render;
mod term;
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
use std::collections::VecDeque;
use std::os::mikros::startup;
use std::rc::Rc;
use term::Term;

/// The `USER` role, and the `id`s of the `TextLogin` and `Seat` channels.
const USER: u32 = 16;
const TEXT_LOGIN_ID: u32 = 3;
const SEAT_ID: u32 = 7;
const SAME_RIGHTS: u32 = 1 << 31;
/// The keyboard layout until `/etc/keyboard.yaml` names one.
const LAYOUT: &str = "us";
/// The rows of the login prompt and of setup's terminal, below the log.
const PROMPT_ROWS: usize = 8;

/// How the screen's rows are shared: the log's rows at the top, and
/// those of the view below them.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Layout {
    log: usize,
    view: usize,
}

/// What the screen shows besides the log.
enum View {
    /// The console's own login prompt.
    Login(Screen),
    /// The terminal of setup or of a session.
    Terminal(Rc<Term>),
}

pub struct Console {
    reactor: Reactor,
    spawner: Spawner,
    directory: DirectoryAsyncClient,
    renderer: RefCell<Option<Renderer>>,
    keyboard: RefCell<Option<Keyboard>>,
    view: RefCell<View>,
    /// Whether the view is the terminal of a session.
    session: Cell<bool>,
    log: LogPane,
    /// Whether Ctrl, Alt and F12 are held.
    log_held: Cell<bool>,
    /// The size of the grid of cells, once a display came.
    grid: Cell<(usize, usize)>,
    /// What was typed while the login prompt shows, for it to read.
    typed: RefCell<VecDeque<u8>>,
    /// Woken when the screen changed, when something was typed for the
    /// prompt, and when the held key changed.
    changed: Rc<Notify>,
    typed_in: Notify,
    keys: Notify,
}

fn now() -> i64 {
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
        match &mut *self.view.borrow_mut() {
            View::Login(screen) => f(screen),
            View::Terminal(term) => f(&mut term.screen.borrow_mut()),
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
        if self.log_held.get() {
            return Layout { log: rows, view: 0 };
        }
        match self.session.get() {
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

    /// The columns and rows of the login prompt and of setup's terminal.
    fn prompt_size(&self) -> (usize, usize) {
        let (cols, rows) = self.grid.get();
        (cols, PROMPT_ROWS.min(rows))
    }

    /// Shows `view`, whole, which is the terminal of a session when
    /// `session` is set.
    fn show(&self, view: View, session: bool) {
        *self.view.borrow_mut() = view;
        self.session.set(session);
        self.with_screen(|s| s.touch());
        self.changed.notify();
    }

    /// Takes in `bytes` typed: the terminal shown gets them, or the login
    /// prompt.
    fn input(self: &Rc<Self>, bytes: Vec<u8>) {
        let term = match &*self.view.borrow() {
            View::Terminal(term) => Some(term.clone()),
            View::Login(_) => None,
        };
        match term {
            Some(term) => {
                let reactor = self.reactor.clone();
                self.spawner
                    .spawn(async move { term.type_in(&reactor, &bytes).await });
            }
            None => {
                self.typed.borrow_mut().extend(bytes);
                self.typed_in.notify();
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
            let log = (layout.log > 0 && (self.log.take_stale() || whole))
                .then(|| self.log.screen(grid.0, layout.log));
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
        let term = match &*self.view.borrow() {
            View::Terminal(term) => Some(term.clone()),
            View::Login(_) => None,
        };
        let rows = self.layout().view;
        match term {
            Some(term) => term.resize(cols, rows).await,
            None => self.with_screen(|s| s.resize(cols, rows)),
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
                match pressed {
                    Some(Pressed::Bytes(bytes)) => self.input(bytes),
                    Some(Pressed::LogView) | Some(Pressed::Nothing) | None => {}
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
            }
        }
    }
}

/// Completes when the first of `a` and `b` completes.
async fn first<A: std::future::Future, B: std::future::Future>(a: A, b: B) {
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
        view: RefCell::new(View::Login(Screen::new(render::MIN_COLS, PROMPT_ROWS))),
        session: Cell::new(false),
        log: LogPane::new(),
        log_held: Cell::new(false),
        grid: Cell::new((render::MIN_COLS, render::MIN_ROWS)),
        typed: RefCell::new(VecDeque::new()),
        changed: Rc::new(Notify::default()),
        typed_in: Notify::default(),
        keys: Notify::default(),
    });
    let text_login = Rc::new(TextLoginAsyncClient::new(
        ClientEnd::new(text_login),
        reactor.clone(),
    ));
    spawner.spawn(console.clone().serve_seat(seat));
    spawner.spawn(console.clone().redraw());
    spawner.spawn(logpane::follow(console.clone()));
    spawner.spawn(console.clone().repeat());
    executor.block_on(login::run(console, text_login));
}
