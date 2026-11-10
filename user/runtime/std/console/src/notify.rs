//! Waiting within the console's one thread until something changed.

use std::cell::RefCell;
use std::future::poll_fn;
use std::task::{Poll, Waker};

#[derive(Default)]
pub struct Notify {
    waiters: RefCell<Vec<Waker>>,
    pending: std::cell::Cell<bool>,
}

impl Notify {
    /// Completes after the next `notify`, or at once when one came since
    /// the last wait.
    pub async fn changed(&self) {
        poll_fn(|cx| {
            if self.pending.replace(false) {
                return Poll::Ready(());
            }
            self.waiters.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    pub fn notify(&self) {
        self.pending.set(true);
        for waker in self.waiters.take() {
            waker.wake();
        }
    }
}
