//! The paged object of a file: the object that clients get to map, whose
//! pages the server lends from the file's own memory object when they are
//! first touched, so that writes through a mapping and through the file
//! show in each other. The server never maps it, so it is unreferenced
//! once every client let go of it, which lets it tell when a file that is
//! run from may change again (MIKSPEC-0049.R42, MIKSPEC-0049.R45).

use crate::service::file::Failed;
use alloc::rc::Rc;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::{Packets, Reactor, Spawner};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;
use mikros_runtrack::{Packet, Tracker};

const PACKET_SIGNAL: u32 = 1;
const PACKET_PAGER: u32 = 3;
/// The signal of a paged object that nothing but the server refers to.
const UNREFERENCED: u32 = 1 << 8;
const PAGER_READ: u32 = 1;
const OP_FAIL: u32 = 1;
const OP_DISCARD: u32 = 6;
const RESIZABLE: u32 = 1 << 0;

/// A file's paged object, and what the server knows of its use.
pub struct Paged {
    /// The server's handle, which may manage the object and wait for it.
    mem: OwnedHandle,
    pager: Rc<OwnedHandle>,
    port: sys::Handle,
    key: u64,
    /// The file's own memory object, which the pages are lent from. It
    /// stays for as long as the paged object does.
    backing: Rc<OwnedHandle>,
    /// The bytes the object covers.
    len: Cell<u64>,
    tracker: RefCell<Tracker>,
    /// The file is gone: the object lasts until it is unreferenced.
    gone: Cell<bool>,
}

impl Paged {
    /// A paged object of `len` bytes for the file whose memory object is
    /// `backing`, answered by a task of its own.
    pub fn create(
        pager: &Rc<OwnedHandle>,
        reactor: &Reactor,
        spawner: &Spawner,
        backing: Rc<OwnedHandle>,
        len: u64,
    ) -> Result<Rc<Paged>, Failed> {
        let packets = reactor.packets();
        let key = packets.key();
        let mut raw = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe {
            sys::pager_create_memory(pager.raw(), reactor.port(), key, len, RESIZABLE, &mut raw)
        };
        if status != mikros_rt::OK {
            return Err(Failed::NoSpace);
        }
        let paged = Rc::new(Paged {
            // SAFETY: the kernel just created the handle for us.
            mem: unsafe { OwnedHandle::from_raw(raw) },
            pager: pager.clone(),
            port: reactor.port(),
            key,
            backing,
            len: Cell::new(len),
            tracker: RefCell::new(Tracker::new()),
            gone: Cell::new(false),
        });
        spawner.spawn(answer(paged.clone(), packets));
        Ok(paged)
    }

    /// A handle with `rights` to the object, which counts as given with
    /// `EXECUTE` and `WRITE` when it has them.
    pub fn handle(&self, rights: u32, execute: bool, write: bool) -> Result<OwnedHandle, Failed> {
        let mut out = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::handle_duplicate(self.mem.raw(), rights, &mut out) };
        if status != mikros_rt::OK {
            return Err(Failed::Io);
        }
        let mut tracker = self.tracker.borrow_mut();
        if tracker.gave(execute, write) {
            self.wait(&mut tracker);
        }
        // SAFETY: the kernel just created the handle for us.
        Ok(unsafe { OwnedHandle::from_raw(out) })
    }

    /// Registers a wait for the object to be unreferenced.
    fn wait(&self, tracker: &mut Tracker) {
        // SAFETY: the call takes no pointers.
        let status =
            unsafe { sys::object_wait_async(self.mem.raw(), self.port, self.key, UNREFERENCED, 0) };
        if status == mikros_rt::OK {
            tracker.register(self.key);
        } else {
            mikros_rt::log!(Error, "tmpfs: waiting for an object failed: {status}");
        }
    }

    /// Registers a wait again when the object is in a state that only its
    /// being unreferenced ends and no wait is registered, which a failed
    /// registration leaves.
    fn keep_waiting(&self, tracker: &mut Tracker) {
        if !tracker.idle() && !tracker.waiting() {
            self.wait(tracker);
        }
    }

    /// Whether the file is run from an object that is in use, so that it
    /// must not change.
    pub fn busy_for_change(&self) -> bool {
        let mut tracker = self.tracker.borrow_mut();
        self.keep_waiting(&mut tracker);
        tracker.busy_for_change()
    }

    /// Whether an object that was given with `WRITE` is in use, so that
    /// none to run from can be given. Nothing is dirty or written back:
    /// the pages are the file's own.
    pub fn busy_for_execute(&self) -> bool {
        let mut tracker = self.tracker.borrow_mut();
        self.keep_waiting(&mut tracker);
        tracker.settle(true);
        tracker.busy_for_execute(true)
    }

    /// Fits the object to the size of the file's memory object, `len`
    /// bytes: a smaller one first loses the pages past it, before the
    /// file shrinks. Returns whether the object has that size now; if not,
    /// it keeps the one it had, and a later call tries again.
    pub fn resize(&self, len: u64) -> bool {
        let old = self.len.get();
        if len < old {
            // SAFETY: the call takes no pointers.
            let status = unsafe {
                sys::pager_op(self.pager.raw(), self.mem.raw(), OP_DISCARD, len, old - len)
            };
            if status != mikros_rt::OK {
                mikros_rt::log!(Error, "tmpfs: dropping pages failed: {status}");
            }
        }
        if len != old {
            // SAFETY: the call takes no pointers.
            let status = unsafe { sys::mem_set_size(self.mem.raw(), len) };
            if status != mikros_rt::OK {
                mikros_rt::log!(Error, "tmpfs: resizing a paged object failed: {status}");
                return false;
            }
            self.len.set(len);
        }
        true
    }

    /// The file is gone. The object and its task end once it is
    /// unreferenced, which a wait of its own makes known if nothing waits
    /// for it now.
    pub fn release(&self) {
        self.gone.set(true);
        let mut tracker = self.tracker.borrow_mut();
        if !tracker.waiting() {
            self.wait(&mut tracker);
        }
    }

    /// Takes in a `SIGNAL` packet. Returns whether the object is gone and
    /// unreferenced, so that the task ends.
    fn signal(&self) -> bool {
        let mut tracker = self.tracker.borrow_mut();
        match tracker.packet(self.key, self.key) {
            Packet::Again(_) => {
                self.wait(&mut tracker);
                false
            }
            Packet::Unreferenced => self.gone.get(),
            Packet::Unknown => false,
        }
    }

    /// Lends the file's pages for the request of `len` bytes at `at`, and
    /// fails it when they cannot be.
    fn lend(&self, at: u64, len: u64) {
        let end = at.saturating_add(len).min(self.len.get());
        if at < end {
            // SAFETY: the call takes no pointers.
            let status = unsafe {
                sys::pager_lend(
                    self.pager.raw(),
                    self.mem.raw(),
                    at,
                    end - at,
                    self.backing.raw(),
                    at,
                )
            };
            if status == mikros_rt::OK {
                return;
            }
            mikros_rt::log!(Error, "tmpfs: lending pages failed: {status}");
        }
        // SAFETY: the call takes no pointers.
        unsafe { sys::pager_op(self.pager.raw(), self.mem.raw(), OP_FAIL, at, len) };
    }
}

/// Answers the page requests of a paged object from the file's memory
/// object until the file is gone and the object unreferenced.
async fn answer(paged: Rc<Paged>, packets: Packets) {
    loop {
        let packet = packets.next().await;
        if packet.kind == PACKET_SIGNAL {
            if paged.signal() {
                return;
            }
            continue;
        }
        let word =
            |at: usize| u64::from_le_bytes(packet.payload[at..at + 8].try_into().expect("8 bytes"));
        let command = u32::from_le_bytes(packet.payload[..4].try_into().expect("4 bytes"));
        if packet.kind != PACKET_PAGER || command != PAGER_READ {
            continue;
        }
        paged.lend(word(8), word(16));
    }
}
