//! The terminals the console makes with the terminal service: the master
//! stream, whose output the console's screen shows and which takes what
//! is typed, and the token that names the terminal. Output that comes in
//! pieces, as a program's redraw larger than the terminal service's
//! chunks does, is drawn once it pauses, not piece by piece. Output that
//! never pauses, from a program that writes without end, is still drawn,
//! and what is typed meanwhile still reaches the terminal: the console
//! takes turns with its other work after each piece it reads.

use crate::emulator::screen::Screen;
use crate::program::notify::Notify;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::terminal::{
    Factory, FactoryAsyncClient, MasterAsyncClient, WindowSize,
};
use mikros_rt::sys;
use std::cell::{Cell, RefCell};
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::rc::Rc;
use std::task::Poll;

const FACTORY: &str = "mikros.terminal.Factory";
const READABLE: u32 = 1 << 0;
const WRITABLE: u32 = 1 << 1;
const PEER_CLOSED: u32 = 1 << 2;
const SHOULD_WAIT: i64 = -22;
const SAME_RIGHTS: u32 = 1 << 31;
const CHUNK: usize = 4096;
/// How long output may pause and still be drawn with what came before
/// it, and the longest the screen goes undrawn while output keeps
/// coming, in nanoseconds.
const SETTLE: i64 = 4_000_000;
const DRAW_INTERVAL: i64 = 33_000_000;

pub struct Term {
    master: OwnedHandle,
    token: OwnedHandle,
    control: MasterAsyncClient,
    pub screen: RefCell<Screen>,
    /// Woken when the screen changed.
    changed: Rc<Notify>,
    closed: Cell<bool>,
}

fn size(cols: usize, rows: usize) -> WindowSize {
    WindowSize {
        rows: rows as u16,
        columns: cols as u16,
        width: 0,
        height: 0,
    }
}

impl Term {
    /// A new terminal of `cols` by `rows`, from the terminal service in
    /// `directory`, whose output changes the screen and wakes `changed`.
    pub async fn open(
        directory: &DirectoryAsyncClient,
        reactor: &Reactor,
        cols: usize,
        rows: usize,
        changed: Rc<Notify>,
    ) -> Option<Rc<Term>> {
        let (client, server) = endpoints::<Factory>().ok()?;
        match directory
            .connect(FACTORY.into(), None, server.into_handle())
            .await
        {
            Ok(Ok(_)) => {}
            _ => return None,
        }
        let factory = FactoryAsyncClient::new(client, reactor.clone());
        let opened = match factory.open(size(cols, rows)).await {
            Ok(Ok(opened)) => opened,
            _ => return None,
        };
        Some(Rc::new(Term {
            master: opened.master,
            token: opened.terminal,
            control: MasterAsyncClient::new(opened.master_control, reactor.clone()),
            screen: RefCell::new(Screen::new(cols, rows)),
            changed,
            closed: Cell::new(false),
        }))
    }

    /// A copy of the terminal's token, to pass on.
    pub fn token(&self) -> Option<OwnedHandle> {
        let mut out = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::handle_duplicate(self.token.raw(), SAME_RIGHTS, &mut out) };
        // SAFETY: the kernel just created the handle for us.
        (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
    }

    /// Changes the window size of the terminal and of its screen.
    pub async fn resize(&self, cols: usize, rows: usize) {
        self.screen.borrow_mut().resize(cols, rows);
        let _ = self.control.set_window_size(size(cols, rows)).await;
        self.changed.notify();
    }

    /// Writes `bytes` to the master stream, as typed.
    pub async fn type_in(&self, reactor: &Reactor, bytes: &[u8]) {
        let mut at = 0;
        while at < bytes.len() && !self.closed.get() {
            let mut actual = 0;
            let rest = &bytes[at..];
            // SAFETY: the pointers describe `rest` and `actual`.
            let status = unsafe {
                sys::stream_write(
                    self.master.raw(),
                    rest.as_ptr(),
                    rest.len() as u64,
                    &mut actual,
                )
            };
            match status {
                mikros_rt::OK => at += actual as usize,
                SHOULD_WAIT => {
                    if reactor
                        .signals(self.master.raw(), WRITABLE | PEER_CLOSED)
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                _ => return,
            }
        }
    }

    /// Reads the master stream into the screen until the terminal closes,
    /// and answers the requests the output makes.
    pub async fn pump(self: Rc<Self>, reactor: Reactor) {
        let mut buffer = [0u8; CHUNK];
        // When the output not drawn yet began to come.
        let mut pending: Option<i64> = None;
        while !self.closed.get() {
            let mut actual = 0;
            // SAFETY: the pointers describe `buffer` and `actual`.
            let status = unsafe {
                sys::stream_read(
                    self.master.raw(),
                    buffer.as_mut_ptr(),
                    CHUNK as u64,
                    &mut actual,
                )
            };
            match status {
                mikros_rt::OK => {
                    let responses = {
                        let mut screen = self.screen.borrow_mut();
                        screen.write(&buffer[..actual as usize]);
                        screen.take_responses()
                    };
                    if !responses.is_empty() {
                        self.type_in(&reactor, &responses).await;
                    }
                    let since = *pending.get_or_insert_with(crate::program::now);
                    if crate::program::now() - since >= DRAW_INTERVAL {
                        pending = None;
                        self.changed.notify();
                    }
                    // While output keeps coming, every read succeeds, and
                    // the console would draw nothing and read no key, ^C
                    // among them, until it stops.
                    yield_now().await;
                }
                SHOULD_WAIT if pending.is_some() => {
                    let settle = crate::program::now() + SETTLE;
                    if !readable_by(&reactor, self.master.raw(), settle).await {
                        pending = None;
                        self.changed.notify();
                    }
                }
                SHOULD_WAIT => {
                    if reactor
                        .signals(self.master.raw(), READABLE | PEER_CLOSED)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                _ => break,
            }
        }
        if pending.is_some() {
            self.changed.notify();
        }
    }

    /// Stops reading: the terminal closes once the console drops it.
    pub fn close(&self) {
        self.closed.set(true);
    }
}

/// Lets the console's other tasks, and the packets queued for them, have
/// a turn before the task goes on.
async fn yield_now() {
    let mut yielded = false;
    poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}

/// Whether `stream` becomes readable, or its writer goes, by `deadline`.
async fn readable_by(reactor: &Reactor, stream: sys::Handle, deadline: i64) -> bool {
    let mut signals = pin!(reactor.signals(stream, READABLE | PEER_CLOSED));
    let mut sleep = pin!(reactor.sleep_until(deadline));
    poll_fn(|cx| {
        if signals.as_mut().poll(cx).is_ready() {
            Poll::Ready(true)
        } else if sleep.as_mut().poll(cx).is_ready() {
            Poll::Ready(false)
        } else {
            Poll::Pending
        }
    })
    .await
}
