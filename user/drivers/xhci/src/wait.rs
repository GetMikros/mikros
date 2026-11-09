//! Waiting within the driver's one thread: for a result that another
//! task gives once, and for a condition that other tasks change.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::poll_fn;
use core::task::{Poll, Waker};

struct Shared<T> {
    value: Option<T>,
    waker: Option<Waker>,
}

/// Where a result is given once.
pub struct Sender<T>(Rc<RefCell<Shared<T>>>);

/// Where that result is awaited.
pub struct Receiver<T>(Rc<RefCell<Shared<T>>>);

pub fn oneshot<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Rc::new(RefCell::new(Shared {
        value: None,
        waker: None,
    }));
    (Sender(shared.clone()), Receiver(shared))
}

impl<T> Sender<T> {
    pub fn send(self, value: T) {
        let waker = {
            let mut shared = self.0.borrow_mut();
            shared.value = Some(value);
            shared.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> Receiver<T> {
    pub async fn recv(self) -> T {
        poll_fn(|cx| {
            let mut shared = self.0.borrow_mut();
            match shared.value.take() {
                Some(value) => Poll::Ready(value),
                None => {
                    shared.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
        .await
    }
}

/// Tasks waiting for a condition to change.
#[derive(Default)]
pub struct Waiters(RefCell<Vec<Waker>>);

impl Waiters {
    /// Waits until `ready` returns a value; it is checked again each
    /// time `wake` is called.
    pub async fn until<T>(&self, mut ready: impl FnMut() -> Option<T>) -> T {
        poll_fn(|cx| match ready() {
            Some(value) => Poll::Ready(value),
            None => {
                self.0.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    pub fn wake(&self) {
        let waiting = core::mem::take(&mut *self.0.borrow_mut());
        for waker in waiting {
            waker.wake();
        }
    }
}
