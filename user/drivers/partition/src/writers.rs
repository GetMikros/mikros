//! The writable connections to the partitions of the disk: at most one
//! opened with `Open`, and the writable locks of the partitions' `bus`
//! connections. While any exists, the driver holds the disk's writable
//! lock on its own `bus` connection to the disk.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_rt::interfaces::mikros::storage::BlockAsyncClient;

pub struct Writers {
    disk: Rc<BlockAsyncClient>,
    open: Cell<bool>,
    locks: Cell<u32>,
    /// Whether the driver holds the disk's lock, and whether it is
    /// asking the disk for it, which those that wait for the answer
    /// wait on.
    held: Cell<bool>,
    asking: Cell<bool>,
    waiting: RefCell<Vec<Waker>>,
}

impl Writers {
    pub fn new(disk: Rc<BlockAsyncClient>) -> Writers {
        Writers {
            disk,
            open: Cell::new(false),
            locks: Cell::new(0),
            held: Cell::new(false),
            asking: Cell::new(false),
            waiting: RefCell::new(Vec::new()),
        }
    }

    /// Counts a writable connection opened with `Open`, unless another is
    /// open, a lock is held or the disk refuses its lock: then false.
    pub async fn open(&self) -> bool {
        if self.open.get() || self.locks.get() > 0 {
            return false;
        }
        self.open.set(true);
        let held = self.hold().await;
        if !held {
            self.open.set(false);
        }
        held
    }

    /// Ends the writable connection that `open` counted.
    pub fn close(&self) {
        self.open.set(false);
        self.release();
    }

    /// Takes a writable lock for a partition's `bus` connection, unless a
    /// writable connection opened with `Open` is open or the disk refuses
    /// its lock: then false.
    pub async fn lock(&self) -> bool {
        if self.open.get() {
            return false;
        }
        self.locks.set(self.locks.get() + 1);
        let held = self.hold().await;
        if !held {
            self.locks.set(self.locks.get() - 1);
        }
        held
    }

    /// Releases a lock that `lock` took.
    pub fn unlock(&self) {
        self.locks.set(self.locks.get().saturating_sub(1));
        self.release();
    }

    /// Whether the driver holds the disk's lock, taking it when it does
    /// not. One request for it is outstanding at a time.
    async fn hold(&self) -> bool {
        poll_fn(|cx| match self.asking.get() {
            false => Poll::Ready(()),
            true => {
                self.waiting.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;
        if !self.held.get() {
            self.asking.set(true);
            let locked = self.disk.lock_writable().await;
            self.held.set(matches!(locked, Ok(Ok(_))));
            self.asking.set(false);
            for waker in self.waiting.borrow_mut().drain(..) {
                waker.wake();
            }
        }
        self.held.get()
    }

    /// Releases the disk's lock once no writable connection remains.
    fn release(&self) {
        let none = !self.open.get() && self.locks.get() == 0;
        if none && self.held.get() && !self.asking.get() {
            self.held.set(false);
            let _ = self.disk.unlock_writable();
        }
    }
}
