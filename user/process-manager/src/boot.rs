//! Reading members of the boot image.

use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys::{self, Handle, Status};

const PAGE_SIZE: u64 = 4096;
const PERM_READ: u32 = 1 << 0;
/// The `object_get_info` topic of memory objects, and its record size.
const INFO_MEMORY: u32 = 4;
const INFO_MEMORY_SIZE: usize = 24;

/// Runs `f` on the bytes of `image` mapped read-only into `process`.
pub fn with_image<T>(
    process: Handle,
    image: &OwnedHandle,
    f: impl FnOnce(&[u8]) -> T,
) -> Result<T, Status> {
    let mut record = [0u8; INFO_MEMORY_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status = unsafe {
        sys::object_get_info(
            image.raw(),
            INFO_MEMORY,
            record.as_mut_ptr(),
            record.len() as u64,
            &mut len,
        )
    };
    if status != mikros_rt::OK {
        return Err(status);
    }
    let size = u64::from_le_bytes(record[..8].try_into().expect("8 bytes"));
    let mapped = size.next_multiple_of(PAGE_SIZE);
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let status = unsafe { sys::vm_map(process, PERM_READ, 0, image.raw(), 0, mapped, &mut at) };
    if status != mikros_rt::OK {
        return Err(status);
    }
    // SAFETY: the image is mapped readable at `at` for `size` bytes, and
    // stays mapped until `f` returns.
    let bytes = unsafe { core::slice::from_raw_parts(at as *const u8, size as usize) };
    let result = f(bytes);
    // SAFETY: nothing refers to the mapping any more.
    unsafe { sys::vm_unmap(process, at, mapped) };
    Ok(result)
}
