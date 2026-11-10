//! One terminal: its line discipline, the master stream, the output
//! queued for it, the readiness events of its connections, and the
//! process session and foreground group it belongs to.

use crate::notify::Notify;
use alloc::collections::VecDeque;
use alloc::rc::{Rc, Weak};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;
use mikros_tty::discipline::{Discipline, Effects, Signal};
use mikros_tty::termios::Attributes;

/// Stream signals, and the event signals of readiness.
const READABLE: u32 = 1 << 0;
const WRITABLE: u32 = 1 << 1;
const PEER_CLOSED: u32 = 1 << 2;
pub const USER0: u32 = 1 << 24;
pub const USER1: u32 = 1 << 25;
const SHOULD_WAIT: i64 = -22;
/// The most output bytes queued for the master stream; writes wait
/// while more are queued.
pub const OUTPUT_MAX: usize = 64 * 1024;
/// How much of the master stream is read at a time.
const CHUNK: usize = 4096;

/// What happened that the terminal's owner acts on: a signal for the
/// foreground group, or the master stream closing.
pub enum Event {
    Signal(Signal),
    WindowChanged,
    HungUp,
}

pub struct Tty {
    pub discipline: RefCell<Discipline>,
    /// The window size: rows, columns, width and height in pixels.
    pub size: Cell<[u16; 4]>,
    master: OwnedHandle,
    output: RefCell<VecDeque<u8>>,
    hung_up: Cell<bool>,
    /// The session whose controlling terminal it is, and its foreground
    /// process group.
    pub session: Cell<Option<u32>>,
    pub foreground: Cell<Option<u32>>,
    /// The readiness events of the connections that asked for one.
    readiness: RefCell<Vec<Weak<OwnedHandle>>>,
    /// Woken when input, output or the foreground group changed.
    pub changed: Notify,
    /// Woken when output is queued for the master stream.
    output_queued: Notify,
    /// Where events for the owner go.
    events: RefCell<VecDeque<Event>>,
    pub event_queued: Notify,
}

impl Tty {
    pub fn new(master: OwnedHandle, size: [u16; 4]) -> Rc<Tty> {
        Rc::new(Tty {
            discipline: RefCell::new(Discipline::new(Attributes::default())),
            size: Cell::new(size),
            master,
            output: RefCell::new(VecDeque::new()),
            hung_up: Cell::new(false),
            session: Cell::new(None),
            foreground: Cell::new(None),
            readiness: RefCell::new(Vec::new()),
            changed: Notify::default(),
            output_queued: Notify::default(),
            events: RefCell::new(VecDeque::new()),
            event_queued: Notify::default(),
        })
    }

    /// Sets the window size, and has `WINCH` sent when it changed.
    pub fn set_size(&self, new: [u16; 4]) {
        if self.size.replace(new) != new {
            self.post(Event::WindowChanged);
        }
    }

    pub fn hung_up(&self) -> bool {
        self.hung_up.get()
    }

    pub fn post(&self, event: Event) {
        self.events.borrow_mut().push_back(event);
        self.event_queued.notify();
    }

    pub fn next_event(&self) -> Option<Event> {
        self.events.borrow_mut().pop_front()
    }

    /// Whether a write may queue output now.
    pub fn writable(&self) -> bool {
        self.hung_up()
            || (self.output.borrow().len() < OUTPUT_MAX
                && !self.discipline.borrow().output_stopped())
    }

    pub fn output_empty(&self) -> bool {
        self.output.borrow().is_empty()
    }

    /// Queues `data`, already processed, for the master stream.
    pub fn queue_output(&self, data: &[u8]) {
        self.output.borrow_mut().extend(data);
        self.output_queued.notify();
        self.update();
    }

    pub fn flush_output(&self) {
        self.output.borrow_mut().clear();
        self.update();
    }

    /// Registers `event` as a readiness event, kept up to date until it
    /// is dropped.
    pub fn add_readiness(&self, event: &Rc<OwnedHandle>) {
        self.readiness.borrow_mut().push(Rc::downgrade(event));
        self.update();
    }

    /// Sets the readiness events' signals, and wakes the waiting calls.
    pub fn update(&self) {
        let readable = self.hung_up() || self.discipline.borrow().readable();
        let mut set = 0;
        if readable {
            set |= USER0;
        }
        if self.writable() {
            set |= USER1;
        }
        let clear = (USER0 | USER1) & !set;
        self.readiness.borrow_mut().retain(|e| {
            let Some(e) = e.upgrade() else { return false };
            // SAFETY: the call takes no pointers.
            unsafe { sys::object_signal(e.raw(), clear, set) };
            true
        });
        self.changed.notify();
    }

    /// Takes in what was typed on the master side.
    fn typed(&self, bytes: &[u8]) {
        for &b in bytes {
            let Effects {
                echo,
                signal,
                output_stopped,
                flushed,
            } = self.discipline.borrow_mut().input(b);
            if flushed {
                self.output.borrow_mut().clear();
            }
            if !echo.is_empty() {
                let echo = self.discipline.borrow_mut().output(&echo);
                self.output.borrow_mut().extend(echo);
                self.output_queued.notify();
            }
            if let Some(signal) = signal {
                self.post(Event::Signal(signal));
            }
            if output_stopped == Some(false) {
                self.output_queued.notify();
            }
        }
        self.update();
    }

    fn hang_up(&self) {
        if !self.hung_up.replace(true) {
            self.post(Event::HungUp);
            self.update();
        }
    }

    /// Reads the master stream until it closes.
    pub async fn read_master(self: Rc<Self>, reactor: Reactor) {
        let mut buffer = [0u8; CHUNK];
        loop {
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
                mikros_rt::OK => self.typed(&buffer[..actual as usize]),
                SHOULD_WAIT => {
                    let waited = reactor
                        .signals(self.master.raw(), READABLE | PEER_CLOSED)
                        .await;
                    if waited.is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
        self.hang_up();
    }

    /// Writes the queued output to the master stream until it closes.
    pub async fn write_master(self: Rc<Self>, reactor: Reactor) {
        loop {
            if self.hung_up() {
                return;
            }
            let chunk: Vec<u8> = {
                let output = self.output.borrow();
                let stopped = self.discipline.borrow().output_stopped();
                if stopped || output.is_empty() {
                    Vec::new()
                } else {
                    output.iter().take(CHUNK).copied().collect()
                }
            };
            if chunk.is_empty() {
                self.output_queued.changed().await;
                continue;
            }
            let mut actual = 0;
            // SAFETY: the pointers describe `chunk` and `actual`.
            let status = unsafe {
                sys::stream_write(
                    self.master.raw(),
                    chunk.as_ptr(),
                    chunk.len() as u64,
                    &mut actual,
                )
            };
            match status {
                mikros_rt::OK => {
                    self.output.borrow_mut().drain(..actual as usize);
                    self.update();
                }
                SHOULD_WAIT => {
                    let waited = reactor
                        .signals(self.master.raw(), WRITABLE | PEER_CLOSED)
                        .await;
                    if waited.is_err() {
                        return;
                    }
                }
                _ => {
                    self.hang_up();
                    return;
                }
            }
        }
    }
}
