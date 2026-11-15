//! Reading the boot disk.

use core::fmt;

/// A read that the firmware failed, with its status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoError(pub usize);

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "reading the disk failed with status {:#x}", self.0)
    }
}

/// Bytes that can be read at any offset.
pub trait Disk {
    /// Fills `buf` with the bytes at `offset`.
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError>;
}

/// The `size` bytes of `disk` from `start`, as a disk of their own. A
/// read that does not lie within them fails with status zero.
pub struct Slice<D> {
    pub disk: D,
    pub start: u64,
    pub size: u64,
}

impl<D: Disk> Disk for Slice<D> {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let end = offset.checked_add(buf.len() as u64).ok_or(IoError(0))?;
        if end > self.size {
            return Err(IoError(0));
        }
        self.disk.read(self.start + offset, buf)
    }
}

#[cfg(test)]
impl Disk for alloc::vec::Vec<u8> {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let at = usize::try_from(offset).map_err(|_| IoError(0))?;
        let bytes = self.get(at..at + buf.len()).ok_or(IoError(0))?;
        buf.copy_from_slice(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn slices_stay_within_their_range() {
        let disk: alloc::vec::Vec<u8> = (0..=255).collect();
        let mut slice = Slice {
            disk,
            start: 16,
            size: 32,
        };
        let mut buf = vec![0; 4];
        slice.read(2, &mut buf).unwrap();
        assert_eq!(buf, [18, 19, 20, 21]);
        slice.read(28, &mut buf).unwrap();
        assert_eq!(slice.read(29, &mut buf), Err(IoError(0)));
        assert_eq!(slice.read(u64::MAX, &mut buf), Err(IoError(0)));
    }
}
