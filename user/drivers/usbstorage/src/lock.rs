//! A lock that tasks of the driver's one thread wait for in turn.

use alloc::collections::VecDeque;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};

#[derive(Default)]
pub struct Lock {
    held: Cell<bool>,
    waiting: RefCell<VecDeque<Waker>>,
}

/// The lock while it is held; dropping it lets the next task have it.
pub struct Held<'a>(&'a Lock);

impl Lock {
    pub async fn lock(&self) -> Held<'_> {
        poll_fn(|cx| {
            if !self.held.replace(true) {
                return Poll::Ready(());
            }
            self.waiting.borrow_mut().push_back(cx.waker().clone());
            Poll::Pending
        })
        .await;
        Held(self)
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.0.held.set(false);
        if let Some(waker) = self.0.waiting.borrow_mut().pop_front() {
            waker.wake();
        }
    }
}
