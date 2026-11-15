//! Hand-offs between the session manager's tasks, without polling: a
//! slot that one task fills once and another waits for, the tasks to
//! wake when something they wait for may have changed, and the first of
//! two futures to complete.

use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::{Poll, Waker};

/// A value that one task gives once and another waits for.
pub struct Slot<T> {
    value: RefCell<Option<T>>,
    given: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}

impl<T> Default for Slot<T> {
    fn default() -> Slot<T> {
        Slot {
            value: RefCell::new(None),
            given: Cell::new(false),
            waker: RefCell::new(None),
        }
    }
}

impl<T> Slot<T> {
    /// Gives `value` and wakes the task that waits for it, unless a value
    /// was given before: whether it was taken.
    pub fn give(&self, value: T) -> bool {
        if self.given.replace(true) {
            return false;
        }
        *self.value.borrow_mut() = Some(value);
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
        true
    }

    /// Waits for the value.
    pub async fn take(&self) -> T {
        poll_fn(|cx| match self.value.borrow_mut().take() {
            Some(value) => Poll::Ready(value),
            None => {
                *self.waker.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}

/// The tasks that wait for something to change.
#[derive(Default)]
pub struct Waiters(RefCell<Vec<Waker>>);

impl Waiters {
    /// Wakes every task that waits, to look again.
    pub fn wake_all(&self) {
        for waker in self.0.take() {
            waker.wake();
        }
    }

    /// Waits until `ready` holds, looking again each time the waiters
    /// are woken.
    pub async fn until(&self, ready: impl Fn() -> bool) {
        poll_fn(|cx| {
            if ready() {
                return Poll::Ready(());
            }
            self.0.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
}

/// Which of two futures completed first.
pub enum Either<A, B> {
    Left(A),
    Right(B),
}

/// The output of whichever of `a` and `b` completes first.
pub async fn select<A: Future, B: Future>(a: A, b: B) -> Either<A::Output, B::Output> {
    let mut a = pin!(a);
    let mut b = pin!(b);
    poll_fn(|cx| {
        if let Poll::Ready(value) = a.as_mut().poll(cx) {
            return Poll::Ready(Either::Left(value));
        }
        if let Poll::Ready(value) = b.as_mut().poll(cx) {
            return Poll::Ready(Either::Right(value));
        }
        Poll::Pending
    })
    .await
}
