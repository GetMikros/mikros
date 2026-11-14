//! The authorization screen on the display: the requests of
//! `OnAuthorize`, one shown at a time over the screen shown when it
//! came, the keys typed into it, and `Answer` and `Cancel`.

use crate::authorize::{Prompt, Request, Step};
use crate::emulator::cell::Cell;
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

    /// Shows the next waiting request over the screen shown now.
    fn show_next(&self) {
        let Some(request) = self.auth.pending.borrow_mut().pop_front() else {
            return;
        };
        let (cols, rows) = self.grid.get();
        let layout = self.layout();
        let top = self.top_screen(cols, layout.log, true);
        let view: Vec<Vec<Cell>> = self.with_screen(|s| {
            (0..layout.view.min(s.rows()))
                .map(|r| (0..cols.min(s.cols())).map(|c| *s.cell(r, c)).collect())
                .collect()
        });
        let cell = |r: usize, c: usize| match (r < layout.log, &top) {
            (true, Some(top)) if r < top.rows() && c < top.cols() => *top.cell(r, c),
            (true, _) => Cell::default(),
            (false, _) => view
                .get(r - layout.log)
                .and_then(|row| row.get(c))
                .copied()
                .unwrap_or_default(),
        };
        *self.auth.shown.borrow_mut() = Some(Prompt::new(request, cols, rows, cell));
        self.changed.notify();
    }

    /// Drops the prompt shown, which erases what was typed into it, and
    /// shows the screen shown before, or the next request.
    fn end_prompt(&self) {
        drop(self.auth.shown.borrow_mut().take());
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
