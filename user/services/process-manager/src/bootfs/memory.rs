//! Memory objects with the contents of boot files: paged objects whose
//! pages the process manager supplies from the image, and private
//! copies.

use super::{Fs, PAGE_SIZE};
use alloc::rc::Rc;
use mikros_rt::exec::Packets;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys::{self, Status};

/// The `PAGER` packet type and its `READ` command.
const PACKET_PAGER: u32 = 3;
const PAGER_READ: u32 = 1;
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
pub(super) fn rights(write: bool, execute: bool) -> u32 {
    let base = DUPLICATE | TRANSFER | READ | MAP | INSPECT;
    base | if write { WRITE } else { 0 } | if execute { EXECUTE } else { 0 }
}

fn owned(status: Status, raw: sys::Handle) -> Result<OwnedHandle, Status> {
    if status != mikros_rt::OK {
        return Err(status);
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(raw) })
}

/// A new anonymous memory object of `len` bytes, a page multiple, that
/// holds the image's bytes from `from` for `count` bytes and zeros after.
pub(super) fn copy(fs: &Fs, from: usize, count: usize, len: u64) -> Result<OwnedHandle, Status> {
    let mut raw = 0;
    // SAFETY: the out pointer is valid for the call.
    let mem = owned(unsafe { sys::mem_create(len, 0, &mut raw) }, raw)?;
    let bytes = &fs.image[from..from + count];
    // SAFETY: the pointer describes `bytes`.
    let status = unsafe { sys::mem_write(mem.raw(), 0, bytes.as_ptr(), bytes.len() as u64) };
    if status != mikros_rt::OK {
        return Err(status);
    }
    Ok(mem)
}

impl Fs {
    /// A private memory object with the contents of the file at
    /// `offset` in the image with `size` bytes, given `rights`.
    pub(super) fn private_memory(
        &self,
        offset: usize,
        size: usize,
        rights: u32,
    ) -> Result<OwnedHandle, Status> {
        let mem = copy(
            self,
            offset,
            size,
            (size as u64).next_multiple_of(PAGE_SIZE),
        )?;
        replace(mem, rights)
    }

    /// A handle with `rights` to the paged object of file `node`, at
    /// `offset` in the image with `size` bytes, created on first use.
    pub(super) fn paged_memory(
        self: &Rc<Self>,
        node: usize,
        offset: usize,
        size: usize,
        rights: u32,
    ) -> Result<OwnedHandle, Status> {
        let existing = self.paged.borrow().get(&node).map(OwnedHandle::raw);
        let raw = match existing {
            Some(raw) => raw,
            None => {
                let packets = self.reactor.packets();
                let mut raw = 0;
                let len = (size as u64).next_multiple_of(PAGE_SIZE);
                // SAFETY: the out pointer is valid for the call.
                let mem = owned(
                    unsafe {
                        sys::pager_create_memory(
                            self.pager.raw(),
                            self.reactor.port(),
                            packets.key(),
                            len,
                            0,
                            &mut raw,
                        )
                    },
                    raw,
                )?;
                let fs = self.clone();
                self.spawner
                    .spawn(async move { fs.supply(packets, node, offset, size).await });
                self.paged.borrow_mut().insert(node, mem);
                raw
            }
        };
        let mut out = 0;
        // SAFETY: the out pointer is valid for the call.
        owned(unsafe { sys::handle_duplicate(raw, rights, &mut out) }, out)
    }

    /// Answers the page requests of file `node`'s paged object for as
    /// long as the process manager runs.
    async fn supply(self: Rc<Self>, packets: Packets, node: usize, offset: usize, size: usize) {
        loop {
            let packet = packets.next().await;
            let word = |at: usize| {
                u64::from_le_bytes(packet.payload[at..at + 8].try_into().expect("8 bytes"))
            };
            let command = u32::from_le_bytes(packet.payload[..4].try_into().expect("4 bytes"));
            if packet.kind != PACKET_PAGER || command != PAGER_READ {
                continue;
            }
            let (at, len) = (word(8), word(16));
            let Some(mem) = self.paged.borrow().get(&node).map(OwnedHandle::raw) else {
                return;
            };
            let start = (at as usize).min(size);
            let count = (len as usize).min(size - start);
            let supplied = copy(&self, offset + start, count, len).and_then(|source| {
                // SAFETY: the call takes no pointers.
                let status =
                    unsafe { sys::pager_supply(self.pager.raw(), mem, at, len, source.raw(), 0) };
                match status {
                    mikros_rt::OK => Ok(()),
                    status => Err(status),
                }
            });
            if let Err(status) = supplied {
                mikros_rt::log!(Error, "supplying a page of a boot file failed: {status}");
            }
        }
    }
}

/// `mem` with only `rights`.
fn replace(mem: OwnedHandle, rights: u32) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call; the kernel closes
    // the old handle.
    let status = unsafe { sys::handle_replace(mem.into_raw(), rights, &mut out) };
    owned(status, out)
}
