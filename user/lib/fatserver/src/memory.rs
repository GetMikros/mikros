//! Memory objects with the contents of files: private copies, and paged
//! objects whose pages the server supplies from the file and drops again
//! when the file changes, so that they show its changes.

use crate::format::Format;
use crate::fs::Fs;
use alloc::rc::Rc;
use alloc::vec;
use mikros_rt::exec::Packets;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::fs::Error;
use mikros_rt::sys;

/// The `PAGER` packet type and its `READ` command, and the pager ops that
/// fail and evict pages.
const PACKET_PAGER: u32 = 3;
const PAGER_READ: u32 = 1;
const OP_FAIL: u32 = 1;
const OP_EVICT: u32 = 2;
const PAGE: u64 = 4096;
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

/// The handle `raw` that a call with `status` made, logging the failure
/// of a call of the server `name`.
fn owned(name: &str, status: sys::Status, raw: sys::Handle) -> Result<OwnedHandle, Error> {
    if status != mikros_rt::OK {
        mikros_rt::log!(Error, "{name}: a memory object call failed: {status}");
        return Err(Error::Io);
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(raw) })
}

/// A new pager for the paged objects of the server `name`.
pub fn pager(name: &str) -> Option<OwnedHandle> {
    let mut raw = 0;
    // SAFETY: the out pointer is valid for the call.
    owned(name, unsafe { sys::pager_create(0, &mut raw) }, raw).ok()
}

impl<F: Format> Fs<F> {
    /// The bytes of the file of node `id` from `at`, `len` of them, with
    /// zeros past its end, in a new anonymous memory object.
    fn contents(&self, id: u64, at: u64, len: u64) -> Result<OwnedHandle, Error> {
        let mut raw = 0;
        // SAFETY: the out pointer is valid for the call.
        let mem = owned(F::NAME, unsafe { sys::mem_create(len, 0, &mut raw) }, raw)?;
        let mut data = vec![0u8; len as usize];
        let n = self.with_node(id, |volume, node| volume.read(node, at, &mut data))?;
        // SAFETY: the pointer describes the first `n` bytes of `data`.
        let status = unsafe { sys::mem_write(mem.raw(), 0, data.as_ptr(), n as u64) };
        if status != mikros_rt::OK {
            return Err(Error::Io);
        }
        Ok(mem)
    }

    /// A private copy of the file of node `id`, given `rights`: a
    /// copy-on-write clone of a copy that the server makes and drops
    /// again, so that the client's object is a clone as `PRIVATE` asks.
    pub fn private_memory(&self, id: u64, rights: u32) -> Result<OwnedHandle, Error> {
        let size = self.with_node(id, |_, node| Ok(F::size(node)))?;
        let len = size.next_multiple_of(PAGE).max(PAGE);
        let copy = self.contents(id, 0, len)?;
        let mut raw = 0;
        // SAFETY: the out pointer is valid for the call.
        let clone = owned(
            F::NAME,
            unsafe { sys::mem_clone(copy.raw(), 0, 0, len, &mut raw) },
            raw,
        )?;
        let mut out = 0;
        // SAFETY: the out pointer is valid for the call; the kernel closes
        // the old handle.
        owned(
            F::NAME,
            unsafe { sys::handle_replace(clone.into_raw(), rights, &mut out) },
            out,
        )
    }

    /// A handle with `rights` to the paged object of the file of node
    /// `id`, created on first use with the file's size then. The object
    /// keeps the node for as long as the server runs.
    pub fn paged_memory(self: &Rc<Self>, id: u64, rights: u32) -> Result<OwnedHandle, Error> {
        let existing = self.paged.borrow().get(&id).map(|(mem, _)| mem.raw());
        let raw = match existing {
            Some(raw) => raw,
            None => {
                let size = self.with_node(id, |_, node| Ok(F::size(node)))?;
                let len = size.next_multiple_of(PAGE).max(PAGE);
                let packets = self.reactor.packets();
                let mut raw = 0;
                // SAFETY: the out pointer is valid for the call.
                let created = unsafe {
                    sys::pager_create_memory(
                        self.pager.raw(),
                        self.reactor.port(),
                        packets.key(),
                        len,
                        0,
                        &mut raw,
                    )
                };
                let mem = owned(F::NAME, created, raw)?;
                self.hold(id);
                let raw = mem.raw();
                self.paged.borrow_mut().insert(id, (mem, len));
                self.spawner.spawn(supply(self.clone(), packets, id));
                raw
            }
        };
        let mut out = 0;
        // SAFETY: the out pointer is valid for the call.
        owned(
            F::NAME,
            unsafe { sys::handle_duplicate(raw, rights, &mut out) },
            out,
        )
    }

    /// Drops the pages of the paged object of node `id`, when it has one,
    /// after its file changed, so that they are asked for again.
    pub fn changed(&self, id: u64) {
        if let Some((mem, len)) = self.paged.borrow().get(&id) {
            // SAFETY: the call takes no pointers.
            unsafe { sys::pager_op(self.pager.raw(), mem.raw(), OP_EVICT, 0, *len) };
        }
    }
}

/// Answers the page requests of the paged object of node `id` from its
/// file for as long as the server runs, failing those of a removed file.
async fn supply<F: Format>(fs: Rc<Fs<F>>, packets: Packets, id: u64) {
    loop {
        let packet = packets.next().await;
        let word =
            |at: usize| u64::from_le_bytes(packet.payload[at..at + 8].try_into().expect("8 bytes"));
        let command = u32::from_le_bytes(packet.payload[..4].try_into().expect("4 bytes"));
        if packet.kind != PACKET_PAGER || command != PAGER_READ {
            continue;
        }
        let (at, len) = (word(8), word(16));
        let Some(mem) = fs.paged.borrow().get(&id).map(|(mem, _)| mem.raw()) else {
            return;
        };
        let status = match fs.contents(id, at, len) {
            // SAFETY: the call takes no pointers.
            Ok(source) => unsafe {
                sys::pager_supply(fs.pager.raw(), mem, at, len, source.raw(), 0)
            },
            // SAFETY: the call takes no pointers.
            Err(_) => unsafe { sys::pager_op(fs.pager.raw(), mem, OP_FAIL, at, len) },
        };
        if status != mikros_rt::OK {
            mikros_rt::log!(Error, "{}: supplying a page failed: {status}", F::NAME);
        }
    }
}
