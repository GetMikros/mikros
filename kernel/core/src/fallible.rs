//! Allocation that reports failure instead of stopping the kernel.

use crate::status::Error;
use alloc::sync::Arc;

/// `value` in a new `Arc`, or `NoMemory` when there is no room for it, in
/// which case `value` is dropped.
pub fn try_arc<T>(value: T) -> Result<Arc<T>, Error> {
    let mut arc = Arc::<T>::try_new_uninit().map_err(|_| Error::NoMemory)?;
    let slot = Arc::get_mut(&mut arc).expect("a new Arc is not shared");
    // Written through the pointer, which copies `value` once even in
    // unoptimized builds, where large values would otherwise take
    // several copies' worth of stack.
    // SAFETY: the slot is valid for writes and holds no value yet.
    unsafe { slot.as_mut_ptr().write(value) };
    // SAFETY: the value was just written.
    Ok(unsafe { arc.assume_init() })
}
