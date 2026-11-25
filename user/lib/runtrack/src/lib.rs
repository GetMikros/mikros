//! What a file server knows of how the memory objects it gave out for one
//! file are in use: whether something runs from the file, and whether
//! something may write it through a mapping. The server asks the kernel to
//! tell it when the object is unreferenced, which the object only becomes
//! when every handle and mapping of it is gone, and only the server can
//! give a new one.
//!
//! The state is that of one paged object, or of the objects of one file
//! taken as one. After the server gave a handle it registers a wait for
//! the object's signal `UNREFERENCED`; a packet of such a registration
//! counts only when the server gave no handle since the registration was
//! made, and then it is registered again.
#![cfg_attr(not(test), no_std)]

/// What a wait that the server registered is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Registration {
    key: u64,
    /// How many handles the server had given when it registered.
    given: u64,
}

/// What the `UNREFERENCED` packets and the handles given say of one file's
/// memory object.
#[derive(Debug, Default)]
pub struct Tracker {
    /// A handle with `EXECUTE` was given, and the object has not been seen
    /// unreferenced since a handle was last given.
    executing: bool,
    /// A handle with `WRITE` was given, and the object has not been seen
    /// unreferenced with nothing dirty since.
    writable: bool,
    /// The object was seen unreferenced, and no handle was given since.
    unreferenced: bool,
    /// How many handles were given.
    given: u64,
    registered: Option<Registration>,
}

/// What a packet for a registration means.
#[derive(Debug, PartialEq, Eq)]
pub enum Packet {
    /// No registration of this tracker has that key.
    Unknown,
    /// The object is unreferenced.
    Unreferenced,
    /// A handle was given since the registration, so the packet says
    /// nothing; the server registers a wait again with this key, and
    /// tells the tracker with `register` once the wait is registered.
    Again(u64),
}

impl Tracker {
    pub const fn new() -> Tracker {
        Tracker {
            executing: false,
            writable: false,
            unreferenced: false,
            given: 0,
            registered: None,
        }
    }

    /// Records that a handle of the object was given, with the right
    /// `EXECUTE` or `WRITE` when those are set. Returns whether the server
    /// has to register a wait now, with `register`.
    pub fn gave(&mut self, execute: bool, write: bool) -> bool {
        self.given += 1;
        self.unreferenced = false;
        self.executing |= execute;
        self.writable |= write;
        self.registered.is_none()
    }

    /// Records that the server registered a wait with `key`, after the
    /// handles it has given so far.
    pub fn register(&mut self, key: u64) {
        self.registered = Some(Registration {
            key,
            given: self.given,
        });
    }

    /// Takes in the `SIGNAL` packet of the registration with `key`.
    pub fn packet(&mut self, key: u64, next_key: u64) -> Packet {
        match self.registered {
            Some(r) if r.key == key => {
                self.registered = None;
                if r.given == self.given {
                    self.unreferenced = true;
                    self.executing = false;
                    Packet::Unreferenced
                } else {
                    Packet::Again(next_key)
                }
            }
            _ => Packet::Unknown,
        }
    }

    /// Whether a call that changes the file must fail with `BUSY`: the
    /// file is run from an object that is in use.
    pub fn busy_for_change(&self) -> bool {
        self.executing
    }

    /// Whether a request for an object to run from must fail with `BUSY`:
    /// an object that was given with `WRITE` is still in use, or has
    /// dirty pages or a writeback, which `clean` says there are none of.
    pub fn busy_for_execute(&self, clean: bool) -> bool {
        self.writable && !(self.unreferenced && clean)
    }

    /// Ends the writable state, once the object is unreferenced and clean.
    pub fn settle(&mut self, clean: bool) {
        if self.unreferenced && clean {
            self.writable = false;
        }
    }

    /// Whether a registered wait is outstanding.
    pub fn waiting(&self) -> bool {
        self.registered.is_some()
    }

    /// Whether the tracker has nothing to remember: it can be dropped.
    pub fn idle(&self) -> bool {
        !self.executing && !self.writable && self.registered.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_to_run_from_makes_the_file_busy_until_the_object_is_unreferenced() {
        let mut t = Tracker::new();
        assert!(!t.busy_for_change());
        assert!(t.gave(true, false));
        t.register(1);
        assert!(t.busy_for_change());
        assert_eq!(t.packet(1, 2), Packet::Unreferenced);
        assert!(!t.busy_for_change());
    }

    #[test]
    fn a_handle_given_after_the_registration_makes_its_packet_count_for_nothing() {
        let mut t = Tracker::new();
        assert!(t.gave(true, false));
        t.register(1);
        // Another handle, which needs no new registration while one waits.
        assert!(!t.gave(false, false));
        assert_eq!(t.packet(1, 2), Packet::Again(2));
        assert!(t.busy_for_change());
        t.register(2);
        assert_eq!(t.packet(2, 3), Packet::Unreferenced);
        assert!(!t.busy_for_change());
    }

    #[test]
    fn a_wait_that_could_not_be_registered_again_is_registered_by_the_next_handle() {
        let mut t = Tracker::new();
        t.gave(true, false);
        t.register(1);
        t.gave(false, false);
        // The packet is stale and the server fails to register again.
        assert_eq!(t.packet(1, 2), Packet::Again(2));
        // The next handle makes the server register a wait.
        assert!(t.gave(false, false));
        t.register(3);
        assert_eq!(t.packet(3, 4), Packet::Unreferenced);
        assert!(!t.busy_for_change());
    }

    #[test]
    fn ignores_a_packet_of_another_registration() {
        let mut t = Tracker::new();
        t.gave(true, false);
        t.register(7);
        assert_eq!(t.packet(8, 9), Packet::Unknown);
        assert!(t.busy_for_change());
    }

    #[test]
    fn a_writable_object_is_busy_for_running_until_it_is_unreferenced_and_clean() {
        let mut t = Tracker::new();
        assert!(!t.busy_for_execute(true));
        t.gave(false, true);
        t.register(1);
        assert!(t.busy_for_execute(true));
        assert_eq!(t.packet(1, 2), Packet::Unreferenced);
        // Unreferenced, but pages are still dirty.
        assert!(t.busy_for_execute(false));
        t.settle(false);
        assert!(t.busy_for_execute(false));
        assert!(!t.busy_for_execute(true));
        t.settle(true);
        assert!(!t.busy_for_execute(false));
        assert!(t.idle());
    }

    #[test]
    fn readers_alone_make_nothing_busy() {
        let mut t = Tracker::new();
        t.gave(false, false);
        t.register(1);
        assert!(!t.busy_for_change() && !t.busy_for_execute(true));
        assert_eq!(t.packet(1, 2), Packet::Unreferenced);
        assert!(t.idle());
    }

    #[test]
    fn a_new_handle_after_unreferenced_makes_the_object_in_use_again() {
        let mut t = Tracker::new();
        t.gave(true, false);
        t.register(1);
        assert_eq!(t.packet(1, 2), Packet::Unreferenced);
        assert!(t.gave(true, false));
        t.register(2);
        assert!(t.busy_for_change());
        assert!(!t.idle());
    }
}
