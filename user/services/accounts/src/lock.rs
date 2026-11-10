//! A lock for the tasks of one executor: at most one task holds it, and
//! the others get it in the order they asked for it.

use alloc::collections::VecDeque;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

#[derive(Default)]
pub struct Lock {
    held: Cell<bool>,
    /// The tasks that wait, by their turn, first to last.
    waiting: RefCell<VecDeque<(u64, Option<Waker>)>>,
    next: Cell<u64>,
}

/// The lock, held until dropped.
pub struct Held<'a>(&'a Lock);

impl Lock {
    pub fn new() -> Lock {
        Lock::default()
    }

    /// Waits for the lock.
    pub fn lock(&self) -> Acquire<'_> {
        let turn = self.next.get();
        self.next.set(turn + 1);
        self.waiting.borrow_mut().push_back((turn, None));
        Acquire {
            lock: self,
            turn,
            done: false,
        }
    }

    /// Whether a task holds the lock or waits for it.
    pub fn busy(&self) -> bool {
        self.held.get() || !self.waiting.borrow().is_empty()
    }

    /// Wakes the first task that waits.
    fn wake_first(&self) {
        if let Some((_, Some(waker))) = self.waiting.borrow().front() {
            waker.wake_by_ref();
        }
    }
}

/// Waiting for a turn of the lock.
pub struct Acquire<'a> {
    lock: &'a Lock,
    turn: u64,
    done: bool,
}

impl<'a> Future for Acquire<'a> {
    type Output = Held<'a>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Held<'a>> {
        let lock = self.lock;
        let mut waiting = lock.waiting.borrow_mut();
        let first = waiting.front().is_some_and(|(t, _)| *t == self.turn);
        if first && !lock.held.get() {
            waiting.pop_front();
            lock.held.set(true);
            self.done = true;
            return Poll::Ready(Held(lock));
        }
        if let Some((_, waker)) = waiting.iter_mut().find(|(t, _)| *t == self.turn) {
            *waker = Some(cx.waker().clone());
        }
        Poll::Pending
    }
}

impl Drop for Acquire<'_> {
    /// A task that stops waiting gives up its turn.
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let was_first = {
            let mut waiting = self.lock.waiting.borrow_mut();
            let at = waiting.iter().position(|(t, _)| *t == self.turn);
            if let Some(at) = at {
                waiting.remove(at);
            }
            at == Some(0)
        };
        if was_first && !self.lock.held.get() {
            self.lock.wake_first();
        }
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.0.held.set(false);
        self.0.wake_first();
    }
}
