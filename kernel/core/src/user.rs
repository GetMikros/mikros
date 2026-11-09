//! User addresses that system calls read from and write to, and the
//! values that may be copied through them.

use crate::status::Error;
use core::marker::PhantomData;

/// Values that can be copied to and from user memory as their bytes.
///
/// # Safety
///
/// Every bit pattern must be a valid value, and the type must have no
/// padding.
pub unsafe trait Plain: Copy {}

// SAFETY: integers have no padding and no invalid bit patterns.
unsafe impl Plain for u8 {}
// SAFETY: as for `u8`.
unsafe impl Plain for u32 {}
// SAFETY: as for `u8`.
unsafe impl Plain for u64 {}
// SAFETY: as for `u8`.
unsafe impl Plain for i32 {}
// SAFETY: as for `u8`.
unsafe impl Plain for i64 {}

/// The bytes of `values`.
pub fn bytes_of<T: Plain>(values: &[T]) -> &[u8] {
    // SAFETY: T is Plain, so it has no padding to leak.
    unsafe { core::slice::from_raw_parts(values.as_ptr().cast::<u8>(), size_of_val(values)) }
}

/// A user address the kernel reads a `T` or an array of them from.
#[allow(dead_code, reason = "no call in the table reads user memory yet")]
pub struct UserIn<T>(pub u64, PhantomData<T>);

#[allow(dead_code, reason = "no call in the table reads user memory yet")]
impl<T: Plain> UserIn<T> {
    pub fn new(addr: u64) -> Self {
        UserIn(addr, PhantomData)
    }

    /// Whether `len` values fit at the address inside the user range.
    pub fn fits(&self, len: u64) -> bool {
        let bytes = len.checked_mul(size_of::<T>() as u64);
        bytes.is_some_and(|b| b == 0 || crate::aspace::in_user_range(self.0, b))
    }

    /// Whether the address is zero.
    pub fn is_null(&self) -> bool {
        self.0 == 0
    }

    /// Copies `dst.len()` values in once.
    pub fn read_into(&self, dst: &mut [T]) -> Result<(), Error> {
        // SAFETY: T is Plain, so its bytes may be written freely.
        let bytes = unsafe {
            core::slice::from_raw_parts_mut(dst.as_mut_ptr().cast::<u8>(), size_of_val(dst))
        };
        crate::arch::user_copy_in(bytes, self.0)
    }

    /// Copies the value in once.
    pub fn read(&self) -> Result<T, Error> {
        let mut value = core::mem::MaybeUninit::<T>::zeroed();
        // SAFETY: T is Plain, so its bytes may be written freely.
        let bytes = unsafe {
            core::slice::from_raw_parts_mut(value.as_mut_ptr().cast::<u8>(), size_of::<T>())
        };
        crate::arch::user_copy_in(bytes, self.0)?;
        // SAFETY: every bit pattern is a valid T.
        Ok(unsafe { value.assume_init() })
    }
}

#[allow(dead_code, reason = "no call in the table reads user bytes yet")]
impl UserIn<u8> {
    /// Copies `dst.len()` bytes in once.
    pub fn read_bytes(&self, dst: &mut [u8]) -> Result<(), Error> {
        crate::arch::user_copy_in(dst, self.0)
    }

    /// The address `offset` bytes further on.
    pub fn at(&self, offset: u64) -> Self {
        UserIn::new(self.0.wrapping_add(offset))
    }
}

/// A user address the kernel writes a `T` or an array of them to.
pub struct UserOut<T>(pub u64, PhantomData<T>);

impl<T: Plain> UserOut<T> {
    pub fn new(addr: u64) -> Self {
        UserOut(addr, PhantomData)
    }

    /// The output range of `len` values at the address. A length that
    /// does not fit in a `u64` saturates, so that the range never fits.
    pub fn range(&self, len: u64) -> (u64, u64) {
        (self.0, len.saturating_mul(size_of::<T>() as u64))
    }

    /// Whether the address is zero.
    pub fn is_null(&self) -> bool {
        self.0 == 0
    }

    /// Whether `len` values fit at the address inside the user range.
    pub fn fits(&self, len: u64) -> bool {
        let bytes = len.checked_mul(size_of::<T>() as u64);
        bytes.is_some_and(|b| b == 0 || crate::aspace::in_user_range(self.0, b))
    }
}

/// A user address the kernel reads from and writes back to.
#[allow(dead_code, reason = "no call in the table has an inout argument yet")]
pub struct UserInOut<T>(u64, PhantomData<T>);

#[allow(dead_code, reason = "no call in the table has an inout argument yet")]
impl<T: Plain> UserInOut<T> {
    pub fn new(addr: u64) -> Self {
        UserInOut(addr, PhantomData)
    }

    pub fn reader(&self) -> UserIn<T> {
        UserIn::new(self.0)
    }

    pub fn writer(&self) -> UserOut<T> {
        UserOut::new(self.0)
    }
}
