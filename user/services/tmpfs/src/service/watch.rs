//! Watches: which connections watch which nodes for which changes, and
//! sending them the events of each change.

use crate::memfs::Id;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::interfaces::__midl::{ClientEnd, Error as MidlError};
use mikros_rt::interfaces::mikros::fs::{WatchEvent, WatchKind, WatchMask, Watcher, WatcherClient};

/// A watch: the connection that set it, by its owner number, the node
/// and changes it watches, its cookie, where events go, and whether
/// events were dropped since the last that could be sent.
struct Watch {
    owner: u64,
    node: Id,
    mask: WatchMask,
    cookie: u64,
    watcher: WatcherClient,
    overflowed: bool,
}

#[derive(Default)]
pub struct Watches {
    list: RefCell<Vec<Watch>>,
    next_move: Cell<u32>,
}

/// One event of a change, as it is sent.
pub fn event(kind: WatchKind, move_cookie: u32, name: &str) -> WatchEvent {
    WatchEvent {
        kind,
        move_cookie,
        name: String::from(name),
    }
}

/// Whether `mask` selects events of `kind`.
fn selects(mask: WatchMask, kind: WatchKind) -> bool {
    let bit = match kind {
        WatchKind::Created => WatchMask::CREATED,
        WatchKind::Removed => WatchMask::REMOVED,
        WatchKind::Modified => WatchMask::MODIFIED,
        WatchKind::Attributes => WatchMask::ATTRIBUTES,
        WatchKind::MovedFrom | WatchKind::MovedTo => WatchMask::MOVED,
        WatchKind::SelfRemoved | WatchKind::SelfMoved => WatchMask::SELF,
        _ => return true,
    };
    mask.contains(bit)
}

impl Watches {
    /// Watches `node` for the changes `mask` selects, for the connection
    /// `owner`, sending events with `cookie` to `watcher`.
    pub fn add(
        &self,
        owner: u64,
        node: Id,
        mask: WatchMask,
        cookie: u64,
        watcher: ClientEnd<Watcher>,
    ) {
        let watcher = WatcherClient::new(watcher);
        self.list.borrow_mut().push(Watch {
            owner,
            node,
            mask,
            cookie,
            watcher,
            overflowed: false,
        });
    }

    /// Ends the watches of `owner` with `cookie`, and whether there was
    /// one.
    pub fn remove(&self, owner: u64, cookie: u64) -> bool {
        let mut list = self.list.borrow_mut();
        let before = list.len();
        list.retain(|w| !(w.owner == owner && w.cookie == cookie));
        list.len() != before
    }

    /// Ends every watch of `owner`.
    pub fn end(&self, owner: u64) {
        self.list.borrow_mut().retain(|w| w.owner != owner);
    }

    /// A new cookie for the two halves of one move, never 0.
    pub fn move_cookie(&self) -> u32 {
        let cookie = self.next_move.get().wrapping_add(1).max(1);
        self.next_move.set(cookie);
        cookie
    }

    /// Sends `events`, those of one change, to each watch of `node`, in
    /// one message, as far as its mask selects them. A watch whose
    /// watcher cannot take a message loses the events until it can, and
    /// then gets one `OVERFLOW` first. A watch whose watcher has closed
    /// ends.
    pub fn notify(&self, node: Id, events: &[WatchEvent]) {
        let mut list = self.list.borrow_mut();
        list.retain_mut(|w| {
            if w.node != node {
                return true;
            }
            let selected: Vec<WatchEvent> = events
                .iter()
                .filter(|e| selects(w.mask, e.kind))
                .map(|e| event(e.kind, e.move_cookie, &e.name))
                .collect();
            if selected.is_empty() {
                return true;
            }
            if w.overflowed {
                match w
                    .watcher
                    .on_events(w.cookie, alloc::vec![event(WatchKind::Overflow, 0, "")])
                {
                    Ok(()) => w.overflowed = false,
                    Err(MidlError::Closed) => return false,
                    Err(_) => return true,
                }
            }
            match w.watcher.on_events(w.cookie, selected) {
                Ok(()) => true,
                Err(MidlError::Closed) => false,
                Err(_) => {
                    w.overflowed = true;
                    true
                }
            }
        });
    }
}
