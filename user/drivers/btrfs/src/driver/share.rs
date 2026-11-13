// SPDX-License-Identifier: GPL-2.0
//! Memory objects of files (MIKSPEC-0049.R7, MIKSPEC-0049.R19,
//! MIKSPEC-0053.R6). Each file that a client asks for one of gets a paged
//! memory object, with dirty tracking and stable writeback, whose pages
//! are lent from the shim's page cache (`pager_lend`), so that the file's
//! readers and writers, through the server and through mappings, share
//! one copy: the shim's `share.c` holds the folios while lent, takes
//! pages back when they leave the page cache, and writes back what
//! clients wrote. A thread of its own answers the objects' page requests,
//! so that the executor may wait for pages, as `mem_clone` does.

use crate::driver::fs::{Fs, Key, Open};
use crate::driver::host::{self, Spin};
use crate::linux::node::Shared;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use core::cell::{Cell, RefCell};
use core::ffi::{c_int, c_long, c_void};
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::fs::Error;
use mikros_rt::sys;

const PAGE: u64 = 4096;
/// The pages lent for a request: the one asked for and those after it,
/// which the shim reads with it when it is not cached, so that a client
/// that reads a mapping in order faults once for each of them.
const WINDOW: u64 = 16;
/// The options of the paged objects: `RESIZABLE`, `TRACK_DIRTY` and
/// `STABLE`.
const OPTIONS: u32 = 1 | 2 | 4;
/// The `PAGER` packet type and its `READ` command, and the pager ops that
/// fail and discard pages.
const PACKET_PAGER: u32 = 3;
const PAGER_READ: u32 = 1;
const OP_FAIL: u32 = 1;
const OP_DISCARD: u32 = 6;
const BAD_STATE: i64 = -20;
const EBUSY: c_int = -16;
const EINVAL: c_int = -22;
const EIO: c_int = -5;
/// Handle rights.
const DUPLICATE: u32 = 1 << 0;
const TRANSFER: u32 = 1 << 1;
const READ: u32 = 1 << 2;
const WRITE: u32 = 1 << 3;
const EXECUTE: u32 = 1 << 4;
const MAP: u32 = 1 << 5;
const INSPECT: u32 = 1 << 9;

/// The rights of a memory object handed out, with `WRITE` and `EXECUTE`
/// only when asked for.
pub fn rights(write: bool, execute: bool) -> u32 {
    let base = DUPLICATE | TRANSFER | READ | MAP | INSPECT;
    base | if write { WRITE } else { 0 } | if execute { EXECUTE } else { 0 }
}

/// The pager of the paged objects and the port of their requests, once
/// the first is made.
static PAGER: AtomicU32 = AtomicU32::new(0);
static PORT: AtomicU32 = AtomicU32::new(0);

/// The paged objects, by cookie, for the thread that answers their
/// requests and the shim's calls, with the shared files they belong to.
static OBJECTS: Spin<BTreeMap<u64, (sys::Handle, Shared)>> = Spin::new(BTreeMap::new());

fn owned(status: sys::Status, raw: sys::Handle) -> Result<OwnedHandle, Error> {
    if status != mikros_rt::OK {
        return Err(Error::Io);
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(raw) })
}

/// A file that clients have a paged object of.
pub struct File {
    mem: OwnedHandle,
    shared: Shared,
    /// The object's size, the file's rounded up to whole pages, or a page.
    len: Cell<u64>,
    /// The file, which stays open as long as the server runs, as the
    /// object does.
    open: Rc<Open>,
    /// Whether a client got the object itself for a copy, as the file
    /// could never change, which it then must not.
    pinned: Cell<bool>,
}

impl File {
    /// A handle with `rights` to the object, for a client that asked for
    /// a copy of a file that can never change, which must stay so.
    pub fn handle_pinned(&self, rights: u32) -> Result<OwnedHandle, Error> {
        self.pinned.set(true);
        self.handle(rights)
    }

    /// A handle with `rights` to the object.
    pub fn handle(&self, rights: u32) -> Result<OwnedHandle, Error> {
        let mut out = 0;
        // SAFETY: the out pointer is valid for the call.
        owned(
            unsafe { sys::handle_duplicate(self.mem.raw(), rights, &mut out) },
            out,
        )
    }

    /// A copy-on-write clone of the object with `rights`, which copies its
    /// lent pages, so that the file's later changes do not show in it.
    pub fn clone_private(&self, rights: u32) -> Result<OwnedHandle, Error> {
        let mut raw = 0;
        let len = self.len.get();
        // SAFETY: the out pointer is valid for the call.
        let clone = owned(
            unsafe { sys::mem_clone(self.mem.raw(), 0, 0, len, &mut raw) },
            raw,
        )?;
        let mut out = 0;
        // SAFETY: the out pointer is valid for the call; the kernel closes
        // the old handle.
        owned(
            unsafe { sys::handle_replace(clone.into_raw(), rights, &mut out) },
            out,
        )
    }

    /// Marks the pages that clients wrote dirty in the page cache.
    pub fn harvest(&self) {
        // SAFETY: the file keeps its node.
        unsafe { self.shared.harvest() };
    }

    /// Fits the object to the file's new size `size`: a smaller one first
    /// loses the pages past it, before the file shrinks.
    pub fn resize(&self, size: u64) {
        let len = size.next_multiple_of(PAGE).max(PAGE);
        let old = self.len.replace(len);
        let pager = PAGER.load(Ordering::Relaxed);
        if len < old {
            // SAFETY: the call takes no pointers.
            unsafe { sys::pager_op(pager, self.mem.raw(), OP_DISCARD, len, old - len) };
        }
        if len != old {
            // SAFETY: the call takes no pointers.
            unsafe { sys::mem_set_size(self.mem.raw(), len) };
        }
    }
}

/// The shared files of the server, by key.
#[derive(Default)]
pub struct Shares {
    files: RefCell<BTreeMap<Key, Rc<File>>>,
    next: Cell<u64>,
}

impl Shares {
    /// The shared file of `key`, if any.
    pub fn get(&self, key: Key) -> Option<Rc<File>> {
        self.files.borrow().get(&key).cloned()
    }

    /// Whether a client got the object itself of a file of the subvolume
    /// with the device `device` for a copy, so that the subvolume must
    /// stay read-only.
    pub fn pins(&self, device: u32) -> bool {
        self.files
            .borrow()
            .values()
            .any(|f| f.pinned.get() && f.open.key.0 == device)
    }

    /// Marks the pages that clients wrote dirty in the page cache, for
    /// every shared file.
    pub fn harvest(&self) {
        let files: alloc::vec::Vec<Rc<File>> = self.files.borrow().values().cloned().collect();
        for file in files {
            file.harvest();
        }
    }
}

impl Fs {
    /// Fits the paged object of `open`, if it has one, to the file's size:
    /// `size` before the file shrinks to it, or the size it grew to.
    pub fn resized(&self, open: &Open, size: Option<u64>) {
        let Some(file) = self.shares.get(open.key) else {
            return;
        };
        let size = match size {
            Some(size) => size,
            None => match open.attributes() {
                Ok(a) => a.size,
                Err(_) => return,
            },
        };
        file.resize(size);
    }

    /// The paged object of the file `open`, made the first time.
    pub fn shared(&self, open: &Rc<Open>) -> Result<Rc<File>, Error> {
        if let Some(file) = self.shares.get(open.key) {
            return Ok(file);
        }
        start()?;
        let size = open.attributes()?.size;
        let len = size.next_multiple_of(PAGE).max(PAGE);
        let cookie = self.shares.next.get() + 1;
        self.shares.next.set(cookie);
        let mut raw = 0;
        let (pager, port) = (PAGER.load(Ordering::Relaxed), PORT.load(Ordering::Relaxed));
        // SAFETY: the out pointer is valid for the call.
        let mem = owned(
            unsafe { sys::pager_create_memory(pager, port, cookie, len, OPTIONS, &mut raw) },
            raw,
        )?;
        let shared = open.node.share(cookie).map_err(crate::driver::fs::error)?;
        OBJECTS.lock().insert(cookie, (mem.raw(), shared));
        let file = Rc::new(File {
            mem,
            shared,
            len: Cell::new(len),
            open: open.clone(),
            pinned: Cell::new(false),
        });
        self.shares
            .files
            .borrow_mut()
            .insert(file.open.key, file.clone());
        Ok(file)
    }
}

/// Makes the pager and the port, and starts the thread that answers the
/// page requests, unless that happened before.
fn start() -> Result<(), Error> {
    if PAGER.load(Ordering::Relaxed) != 0 {
        return Ok(());
    }
    let (mut pager, mut port) = (0, 0);
    // SAFETY: the out pointers are valid for the calls.
    unsafe {
        if sys::pager_create(0, &mut pager) != mikros_rt::OK {
            return Err(Error::Io);
        }
        if sys::port_create(0, &mut port) != mikros_rt::OK {
            sys::handle_close(pager);
            return Err(Error::Io);
        }
    }
    PAGER.store(pager, Ordering::Relaxed);
    PORT.store(port, Ordering::Relaxed);
    if !host::start_thread(answer, core::ptr::null_mut()) {
        return Err(Error::NoSpace);
    }
    Ok(())
}

/// Answers the page requests of the paged objects for as long as the
/// server runs: lends each page from the page cache, or fails it.
extern "C" fn answer(_: *mut c_void) {
    let (pager, port) = (PAGER.load(Ordering::Relaxed), PORT.load(Ordering::Relaxed));
    let word = |p: &[u8], at: usize| u64::from_le_bytes(p[at..at + 8].try_into().expect("8 bytes"));
    let half = |p: &[u8], at: usize| u32::from_le_bytes(p[at..at + 4].try_into().expect("4 bytes"));
    loop {
        let mut packet = [0u8; 48];
        // SAFETY: the pointer describes `packet`, a whole packet.
        if unsafe { sys::port_wait(port, i64::MAX, packet.as_mut_ptr()) } != mikros_rt::OK {
            continue;
        }
        if half(&packet, 8) != PACKET_PAGER || half(&packet, 16) != PAGER_READ {
            continue;
        }
        let (cookie, offset, len) = (word(&packet, 0), word(&packet, 24), word(&packet, 32));
        let Some((mem, shared)) = OBJECTS.lock().get(&cookie).copied() else {
            continue;
        };
        // The pages after those asked for, unless they are past the end,
        // are lent first, so that the client finds them once it goes on,
        // but never failed; lending skips those present.
        let end = offset + len;
        for at in (end..end + (WINDOW - 1) * PAGE).step_by(PAGE as usize) {
            if !lend(pager, mem, shared, at) {
                break;
            }
        }
        let mut at = offset;
        while at < end {
            if !lend(pager, mem, shared, at) {
                // SAFETY: the call takes no pointers.
                unsafe { sys::pager_op(pager, mem, OP_FAIL, at, PAGE) };
            }
            at += PAGE;
        }
    }
}

/// Lends the page at `at` of the object `mem` of `shared` from the page
/// cache: false past the end of the file, or when it cannot be read or
/// lent.
fn lend(pager: sys::Handle, mem: sys::Handle, shared: Shared, at: u64) -> bool {
    // SAFETY: the server keeps the nodes of shared files.
    match unsafe { shared.lend_page(at / PAGE) } {
        Ok(page) => {
            let status = host::backing(page as u64).map(|(source, offset)| {
                // SAFETY: the call takes no pointers.
                unsafe { sys::pager_lend(pager, mem, at, PAGE, source, offset) }
            });
            // SAFETY: as above.
            unsafe { shared.lent() };
            status == Some(mikros_rt::OK)
        }
        Err(_) => false,
    }
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_share_op(cookie: u64, op: u32, first: u64, count: u64) -> c_int {
    let Some((mem, _)) = OBJECTS.lock().get(&cookie).copied() else {
        return EINVAL;
    };
    let pager = PAGER.load(Ordering::Relaxed);
    // SAFETY: the call takes no pointers.
    match unsafe { sys::pager_op(pager, mem, op, first * PAGE, count * PAGE) } {
        mikros_rt::OK => 0,
        BAD_STATE => EBUSY,
        _ => EIO,
    }
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_share_dirty(
    cookie: u64,
    first: u64,
    count: u64,
    ranges: *mut u64,
    cap: c_long,
) -> c_long {
    let Some((mem, _)) = OBJECTS.lock().get(&cookie).copied() else {
        return EINVAL.into();
    };
    let pager = PAGER.load(Ordering::Relaxed);
    let mut n = 0;
    // SAFETY: the shim's buffer holds `cap` pairs of words.
    let status = unsafe {
        sys::pager_query_dirty(
            pager,
            mem,
            first * PAGE,
            count * PAGE,
            ranges.cast(),
            cap as u64,
            &mut n,
        )
    };
    if status != mikros_rt::OK {
        return EIO.into();
    }
    // The kernel's byte offsets and lengths become pages.
    for i in 0..(n as usize) * 2 {
        // SAFETY: the kernel stored `n` pairs.
        unsafe { *ranges.add(i) /= PAGE };
    }
    n as c_long
}
