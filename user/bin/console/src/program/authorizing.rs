//! The authorization screen on the display: the requests of
//! `OnAuthorize`, one shown at a time over the terminal of the console
//! it names, the keys typed into it, `Answer` and `Cancel`, and the
//! lines that the terminal keeps of it.

use crate::authorize::{Prompt, Request, Step};
use crate::program::{Console, now};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::session::TextLoginAsyncClient;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/// How long the popup says that the authorization failed.
const FAILED_FOR: i64 = 2_000_000_000;

#[derive(Default)]
pub struct Authorizing {
    /// The prompt shown, and the requests that wait for it to end.
    pub shown: RefCell<Option<Prompt>>,
    pending: RefCell<VecDeque<Request>>,
    pub text_login: RefCell<Option<Rc<TextLoginAsyncClient>>>,
}

impl Console {
    /// Whether the authorization screen shows.
    pub(super) fn authorizing(&self) -> bool {
        self.auth.shown.borrow().is_some()
    }

    /// Shows `request` at once, or after the request shown ends.
    pub(super) fn authorize(&self, request: Request) {
        self.auth.pending.borrow_mut().push_back(request);
        if !self.authorizing() {
            self.show_next();
        }
    }

    /// Ends request `request`, shown or waiting, without an answer.
    pub(super) fn authorize_ended(&self, request: u32) {
        self.auth
            .pending
            .borrow_mut()
            .retain(|r| r.request != request);
        let shown = self.auth.shown.borrow().as_ref().map(|p| p.request.request);
        if shown == Some(request) {
            self.end_prompt();
        }
    }

    /// The text console with number `console`, 1 to 6.
    fn vt_of(&self, console: u8) -> Option<&Rc<crate::program::vt::Vt>> {
        self.vts.get(usize::from(console).checked_sub(1)?)
    }

    /// Shows the next waiting request over the screen of the terminal of
    /// its console, which a request of a console without a session
    /// cannot have.
    fn show_next(&self) {
        let Some(request) = self.auth.pending.borrow_mut().pop_front() else {
            return;
        };
        let Some(vt) = self.vt_of(request.console).cloned() else {
            return self.show_next();
        };
        let prompt = vt.with_screen(|s| {
            let (cols, rows) = (s.cols(), s.rows());
            let cursor = s.cursor_position();
            let cell = |r: usize, c: usize| *s.cell(r, c);
            Prompt::new(request, cols, rows, cursor, cell)
        });
        *self.auth.shown.borrow_mut() = Some(prompt);
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
            vt.with_screen(|s| s.write(kept.as_bytes()));
        }
    }

    /// Drops the prompt shown, which erases what was typed into it, after
    /// writing the lines it showed to its terminal's screen, and shows the
    /// screen shown before, or the next request.
    fn end_prompt(&self) {
        self.keep_lines();
        let Some(prompt) = self.auth.shown.borrow_mut().take() else {
            return;
        };
        drop(prompt);
        self.with_screen(|s| s.touch());
        self.changed.notify();
        self.show_next();
    }

    /// Takes in `bytes` typed while the authorization screen shows.
    pub(super) fn authorize_key(self: &Rc<Self>, bytes: &[u8]) {
        let step = match self.auth.shown.borrow_mut().as_mut() {
            Some(prompt) => (prompt.key(bytes), prompt.request.request),
            None => return,
        };
        self.changed.notify();
        let Some(text_login) = self.auth.text_login.borrow().clone() else {
            return;
        };
        match step {
            (Step::Nothing, _) => {}
            (Step::Cancel, request) => {
                self.end_prompt();
                self.spawner.spawn(async move {
                    let _ = text_login.cancel(request).await;
                });
            }
            (Step::Answer(name, password), request) => {
                let console = self.clone();
                self.spawner.spawn(async move {
                    let answered = text_login.answer(request, name, password).await;
                    if matches!(answered, Ok(Err(Status::AccessDenied))) {
                        if let Some(prompt) = console.auth.shown.borrow_mut().as_mut() {
                            prompt.failed();
                        }
                        // The client learns the answer now, and may write
                        // at once: the lines go first.
                        console.keep_lines();
                        console.changed.notify();
                        let _ = console.reactor.sleep_until(now() + FAILED_FOR).await;
                    }
                    let shown = console
                        .auth
                        .shown
                        .borrow()
                        .as_ref()
                        .map(|p| p.request.request);
                    if shown == Some(request) {
                        console.end_prompt();
                    }
                });
            }
        }
    }
}
