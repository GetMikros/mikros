//! Pinned memory for buffers and requests.

use alloc::vec;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;

pub const PAGE: u64 = 4096;
pub(crate) const DEVICE_READ: u32 = 1 << 0;
pub(crate) const DEVICE_WRITE: u32 = 1 << 1;
/// The option of `mem_create_contiguous` and `mem_create_pinned` that
/// keeps the memory below 4 GiB.
pub(crate) const BELOW_4G: u32 = 1 << 0;

pub(crate) fn kernel(status: i64) -> Result<(), Status> {
    match status {
        mikros_rt::OK => Ok(()),
        status => Err(Status::from_raw(status as i32)),
    }
}

/// A pinned memory object of `size` bytes from the `DMA` resource `dma`,
/// pinned for the device to read and write through `bti`, and the pin.
pub fn pinned(
    dma: &OwnedHandle,
    bti: &OwnedHandle,
    size: u64,
) -> Result<(OwnedHandle, OwnedHandle), Status> {
    pinned_with(dma, bti, size, false)
}

/// As `pinned`, with the memory below 4 GiB when `below_4g` is true.
pub fn pinned_with(
    dma: &OwnedHandle,
    bti: &OwnedHandle,
    size: u64,
    below_4g: bool,
) -> Result<(OwnedHandle, OwnedHandle), Status> {
    let options = if below_4g { BELOW_4G } else { 0 };
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    kernel(unsafe { sys::mem_create_pinned(dma.raw(), size, options, &mut mem) })?;
    // SAFETY: the kernel just created the handle for us.
    let mem = unsafe { OwnedHandle::from_raw(mem) };
    let mut pmt = 0;
    // SAFETY: the out pointer is valid for the call.
    kernel(unsafe {
        sys::bti_pin(
            bti.raw(),
            mem.raw(),
            0,
            size,
            DEVICE_READ | DEVICE_WRITE,
            &mut pmt,
        )
    })?;
    // SAFETY: the kernel just created the handle for us.
    Ok((mem, unsafe { OwnedHandle::from_raw(pmt) }))
}

/// The physical addresses of the `count` pages of the pinned or
/// contiguous memory object `mem`.
pub fn pages(mem: &OwnedHandle, count: u64) -> Result<Vec<u64>, Status> {
    let mut pages = vec![0u64; count as usize];
    for (i, chunk) in pages.chunks_mut(512).enumerate() {
        // SAFETY: the pointer describes `chunk`.
        kernel(unsafe {
            sys::mem_get_physical_pages(
                mem.raw(),
                512 * i as u64,
                chunk.len() as u64,
                chunk.as_mut_ptr(),
            )
        })?;
    }
    Ok(pages)
}
