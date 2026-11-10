//! Which writable connections a disk has: at most one opened with
//! `Open`, and the writable locks of its `bus` connections.

use alloc::rc::Rc;
use core::cell::Cell;

/// The writable connections of one disk, shared by its `bus` connections
/// and its raw access service. A driver makes one for each disk and
/// passes it to `serve_bus` and `serve_provider`.
#[derive(Clone, Default)]
pub struct Writer(Rc<State>);

#[derive(Default)]
struct State {
    open: Cell<bool>,
    locks: Cell<u32>,
}

impl Writer {
    /// Counts a writable connection opened with `Open`, unless another
    /// is open or a lock is held: then false.
    pub(crate) fn open(&self) -> bool {
        let free = !self.0.open.get() && self.0.locks.get() == 0;
        if free {
            self.0.open.set(true);
        }
        free
    }

    /// Ends the writable connection that `open` counted.
    pub(crate) fn close(&self) {
        self.0.open.set(false);
    }

    /// Takes a writable lock, unless a writable connection opened with
    /// `Open` is open: then false.
    pub(crate) fn lock(&self) -> bool {
        let free = !self.0.open.get();
        if free {
            self.0.locks.set(self.0.locks.get() + 1);
        }
        free
    }

    /// Releases a lock that `lock` took.
    pub(crate) fn unlock(&self) {
        self.0.locks.set(self.0.locks.get().saturating_sub(1));
    }
}
