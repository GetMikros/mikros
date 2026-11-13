//! One of the console's text consoles, numbered 1 to 6: what it shows
//! besides the log, whether that is the terminal of a session, what was
//! typed at its login prompt, and when its session ended. A console that
//! is not shown keeps all of it.

use crate::notify::Notify;
use crate::term::Term;
use mikros_vt::screen::Screen;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

/// What a console shows besides the log.
pub enum View {
    /// The console's own login prompt.
    Login(Screen),
    /// The terminal of setup or of a session.
    Terminal(Rc<Term>),
}

pub struct Vt {
    /// Its number in `TextLogin`.
    pub number: u8,
    pub view: RefCell<View>,
    /// Whether the view is the terminal of a session.
    pub session: Cell<bool>,
    /// Whether it shows only the log, as consoles 2 to 6 do while the
    /// first setup runs on console 1.
    pub log_only: Cell<bool>,
    /// What was typed while the login prompt shows, for it to read.
    pub typed: RefCell<VecDeque<u8>>,
    pub typed_in: Notify,
    /// Woken when `OnSessionEnded` names it.
    pub ended: Notify,
}

impl Vt {
    /// Console `number`, with an empty login prompt of `cols` by `rows`.
    pub fn new(number: u8, cols: usize, rows: usize) -> Rc<Vt> {
        Rc::new(Vt {
            number,
            view: RefCell::new(View::Login(Screen::new(cols, rows))),
            session: Cell::new(false),
            log_only: Cell::new(false),
            typed: RefCell::new(VecDeque::new()),
            typed_in: Notify::default(),
            ended: Notify::default(),
        })
    }

    /// The console's screen, for `f`.
    pub fn with_screen<T>(&self, f: impl FnOnce(&mut Screen) -> T) -> T {
        match &mut *self.view.borrow_mut() {
            View::Login(screen) => f(screen),
            View::Terminal(term) => f(&mut term.screen.borrow_mut()),
        }
    }

    /// The terminal it shows, if it shows one.
    pub fn term(&self) -> Option<Rc<Term>> {
        match &*self.view.borrow() {
            View::Terminal(term) => Some(term.clone()),
            View::Login(_) => None,
        }
    }
}
