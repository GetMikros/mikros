//! Memory objects with the contents of boot files: paged objects whose
//! pages the pager's thread supplies from the image, and private copies.

use super::{Fs, PAGE_SIZE};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys::{self, Status};

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
    copy_bytes(&fs.image[from..from + count], len)
}

/// A new anonymous memory object of `len` bytes, a page multiple, that
/// holds `bytes` and zeros after.
pub(super) fn copy_bytes(bytes: &[u8], len: u64) -> Result<OwnedHandle, Status> {
    let mut raw = 0;
    // SAFETY: the out pointer is valid for the call.
    let mem = owned(unsafe { sys::mem_create(len, 0, &mut raw) }, raw)?;
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

    /// A handle with `rights` to the paged object of file `node`, with
    /// `size` bytes, created on first use. Its pages come from the
    /// pager's thread (`super::pager`).
    pub(super) fn paged_memory(
        &self,
        node: usize,
        size: usize,
        rights: u32,
    ) -> Result<OwnedHandle, Status> {
        let len = (size as u64).next_multiple_of(PAGE_SIZE);
        let raw = self.pager.object(node, len)?;
        let mut out = 0;
        // SAFETY: the out pointer is valid for the call.
        owned(unsafe { sys::handle_duplicate(raw, rights, &mut out) }, out)
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
