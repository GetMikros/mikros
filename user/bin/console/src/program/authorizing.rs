//! The authorization screen on the display, as one state machine: the
//! requests of `OnAuthorize` wait their turn and show one at a time over
//! the terminal of the console they name, and every event about them,
//! keys, answers, ends and timers, goes through `apply`, which acts on
//! it only while its request is the one shown. Showing a request gives it
//! the keyboard through the input router, and ending it gives the
//! keyboard back, so keys typed meanwhile reach no program.

use crate::authorize::{Prompt, Request, Step};
use crate::input::{Owner, SecureKey};
use crate::program::{Console, now};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::session::TextLoginAsyncClient;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/// How long the lines say that the authorization failed.
const FAILED_FOR: i64 = 2_000_000_000;

#[derive(Default)]
pub struct Authorizing {
    /// The prompt shown, which only `apply` changes, and the requests
    /// that wait for it to end.
    pub shown: RefCell<Option<Prompt>>,
    pending: RefCell<VecDeque<Request>>,
    pub text_login: RefCell<Option<Rc<TextLoginAsyncClient>>>,
}

/// What happens to the requests.
pub enum Event {
    /// `OnAuthorize`.
    Asked(Request),
    /// `OnAuthorizeEnded`.
    Ended(u32),
    /// A key that the input router gave the request.
    Key(u32, SecureKey),
    /// The reply to `Answer`: whether the credentials were refused.
    Answered(u32, bool),
    /// The failure has been shown long enough.
    FailureShown(u32),
}

impl Console {
    /// Whether the authorization screen shows.
    pub(super) fn authorizing(&self) -> bool {
        self.auth.shown.borrow().is_some()
    }

    /// The number of the request shown.
    fn shown_request(&self) -> Option<u32> {
        self.auth.shown.borrow().as_ref().map(|p| p.request.request)
    }

    /// Takes in a key that the input router gave request `request`.
    pub(super) fn authorize_key(self: &Rc<Self>, request: u32, key: SecureKey) {
        self.apply(Event::Key(request, key));
    }

    /// Acts on `event`, which concerns the request shown or a waiting
    /// one; an event of a request that ended does nothing.
    pub(super) fn apply(self: &Rc<Self>, event: Event) {
        let shown = self.shown_request();
        match event {
            Event::Asked(request) => {
                self.auth.pending.borrow_mut().push_back(request);
                if shown.is_none() {
                    self.show_next();
                }
            }
            Event::Ended(request) => {
                self.auth
                    .pending
                    .borrow_mut()
                    .retain(|r| r.request != request);
                if shown == Some(request) {
                    self.end_prompt();
                }
            }
            Event::Key(request, key) if shown == Some(request) => {
                let step = match self.auth.shown.borrow_mut().as_mut() {
                    Some(prompt) => prompt.key(&key),
                    None => Step::Nothing,
                };
                drop(key);
                self.changed.notify();
                match step {
                    Step::Nothing => {}
                    Step::Cancel => {
                        self.end_prompt();
                        if let Some(text_login) = self.auth.text_login.borrow().clone() {
                            self.spawner.spawn(async move {
                                let _ = text_login.cancel(request).await;
                            });
                        }
                    }
                    Step::Answer(name, password) => self.answer(request, name, password),
                }
            }
            Event::Answered(request, refused) if shown == Some(request) => {
                if !refused {
                    return self.end_prompt();
                }
                if let Some(prompt) = self.auth.shown.borrow_mut().as_mut() {
                    prompt.failed();
                }
                // The client learns the answer now, and may write at
                // once: the lines go first.
                self.keep_lines();
                self.changed.notify();
                let console = self.clone();
                self.spawner.spawn(async move {
                    let _ = console.reactor.sleep_until(now() + FAILED_FOR).await;
                    console.apply(Event::FailureShown(request));
                });
            }
            Event::FailureShown(request) if shown == Some(request) => self.end_prompt(),
            Event::Key(..) | Event::Answered(..) | Event::FailureShown(_) => {}
        }
    }

    /// Calls `Answer` for request `request`, and applies its reply.
    fn answer(self: &Rc<Self>, request: u32, name: String, password: String) {
        let Some(text_login) = self.auth.text_login.borrow().clone() else {
            return;
        };
        let console = self.clone();
        self.spawner.spawn(async move {
            let answered = text_login.answer(request, name, password).await;
            let refused = matches!(answered, Ok(Err(Status::AccessDenied)));
            console.apply(Event::Answered(request, refused));
        });
    }

    /// The text console with number `console`, 1 to 6.
    fn vt_of(&self, console: u8) -> Option<&Rc<crate::program::vt::Vt>> {
        self.vts.get(usize::from(console).checked_sub(1)?)
    }

    /// Shows the next waiting request over the normal screen of the
    /// terminal of its console, which a request of a console without a
    /// session cannot have: the screen shown, or, while a program shows
    /// the alternate screen, the one it left, which keeps the lines. The
    /// request gets the keyboard.
    fn show_next(&self) {
        let Some(request) = self.auth.pending.borrow_mut().pop_front() else {
            return;
        };
        let Some(vt) = self.vt_of(request.console).cloned() else {
            return self.show_next();
        };
        let id = request.request;
        let prompt = vt.with_screen(|s| {
            let (cols, rows) = (s.cols(), s.rows());
            let cursor = s.normal_cursor();
            let cell = |r: usize, c: usize| *s.normal_cell(r, c);
            Prompt::new(request, cols, rows, cursor, cell)
        });
        *self.auth.shown.borrow_mut() = Some(prompt);
        self.router.borrow_mut().set_owner(Owner::Secure(id), now());
        self.changed.notify();
    }

    /// Writes the lines of the prompt shown to its terminal's screen,
    /// once, before what the terminal's programs write next.
    fn keep_lines(&self) {
        let mut shown = self.auth.shown.borrow_mut();
        let Some(prompt) = shown.as_mut() else {
            return;
        };
        let console = prompt.request.console;
        if let (Some(kept), Some(vt)) = (prompt.take_transcript(), self.vt_of(console)) {
            vt.with_screen(|s| s.write_normal(kept.as_bytes()));
        }
    }

    /// Ends the prompt shown: writes its lines to its terminal's screen,
    /// gives the keyboard back, drops the prompt, which erases what was
    /// typed into it, and shows the screen shown before, or the next
    /// request.
    fn end_prompt(&self) {
        self.keep_lines();
        self.router.borrow_mut().set_owner(Owner::Terminal, now());
        let prompt = self.auth.shown.borrow_mut().take();
        drop(prompt);
        self.with_screen(|s| s.touch());
        self.changed.notify();
        self.show_next();
    }
}
