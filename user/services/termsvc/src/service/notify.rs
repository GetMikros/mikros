//! Waiting within the service's one thread until something changed.

use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::poll_fn;
use core::task::{Poll, Waker};

/// Tasks waiting for a change.
#[derive(Default)]
pub struct Notify(RefCell<Vec<Waker>>);

impl Notify {
    /// Completes after the next `notify`.
    pub async fn changed(&self) {
        let mut waiting = false;
        poll_fn(|cx| {
            if waiting {
                return Poll::Ready(());
            }
            waiting = true;
            self.0.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    pub fn notify(&self) {
        for waker in core::mem::take(&mut *self.0.borrow_mut()) {
            waker.wake();
        }
    }
}
