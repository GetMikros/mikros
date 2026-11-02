//! Preparation and publication of bounded batches of syscall outputs.

use kernel_core::status::Error;

pub type Output<'a> = (u64, &'a [u8]);

pub trait Memory {
    type Locked<'a>: Locked
    where
        Self: 'a;

    /// Resolve writable backing without modifying the destination bytes.
    fn resolve(&self, addr: u64, len: usize) -> Result<(), Error>;
    fn lock(&self) -> Self::Locked<'_>;
}

pub trait Locked {
    /// Recheck after resolving all outputs, with their backing held stable.
    fn writable(&mut self, addr: u64, len: usize) -> bool;
    /// Publish a checked range without allocation, blocking or fault resolution.
    ///
    /// # Safety
    ///
    /// `writable` must have returned true for this entire range while this
    /// guard held the backing stable; the active address space must not change.
    unsafe fn write(&mut self, addr: u64, bytes: &[u8]);
}

/// The caller must bound the batch so publication needs no reschedule point.
/// Outputs may overlap; writes occur in the supplied order.
pub fn write(memory: &impl Memory, outputs: &[Output<'_>]) -> Result<(), Error> {
    for &(addr, bytes) in outputs {
        if !bytes.is_empty() {
            memory.resolve(addr, bytes.len())?;
        }
    }
    let mut locked = memory.lock();
    for &(addr, bytes) in outputs {
        if !bytes.is_empty() && !locked.writable(addr, bytes.len()) {
            return Err(Error::InvalidArgs);
        }
    }
    for &(addr, bytes) in outputs {
        if !bytes.is_empty() {
            // SAFETY: every range passed writable under this same guard.
            // Publication neither blocks nor switches address spaces.
            unsafe { locked.write(addr, bytes) };
        }
    }
    Ok(())
}

/// Publish an information record, or just its size when capacity is short.
pub fn information(
    memory: &impl Memory,
    buffer: u64,
    capacity: u64,
    record: &[u8],
    out_len: u64,
) -> Result<(), Error> {
    let size = record.len() as u64;
    let size_bytes = size.to_ne_bytes();
    if capacity < size {
        write(memory, &[(out_len, &size_bytes)])?;
        return Err(Error::BufferTooSmall);
    }
    write(memory, &[(buffer, record), (out_len, &size_bytes)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{RefCell, RefMut};

    struct Model {
        bytes: RefCell<[u8; 16]>,
        resolve_end: usize,
        locked_end: usize,
    }

    struct Guard<'a> {
        bytes: RefMut<'a, [u8; 16]>,
        end: usize,
    }

    fn fits(addr: u64, len: usize, end: usize) -> bool {
        addr.checked_add(len as u64)
            .is_some_and(|n| n <= end as u64)
    }

    impl Memory for Model {
        type Locked<'a> = Guard<'a>;

        fn resolve(&self, addr: u64, len: usize) -> Result<(), Error> {
            if fits(addr, len, self.resolve_end) {
                Ok(())
            } else {
                Err(Error::InvalidArgs)
            }
        }

        fn lock(&self) -> Guard<'_> {
            Guard {
                bytes: self.bytes.borrow_mut(),
                end: self.locked_end,
            }
        }
    }

    impl Locked for Guard<'_> {
        fn writable(&mut self, addr: u64, len: usize) -> bool {
            fits(addr, len, self.end)
        }

        // SAFETY: the caller checked the full range under this guard.
        unsafe fn write(&mut self, addr: u64, bytes: &[u8]) {
            let start = addr as usize;
            self.bytes[start..start + bytes.len()].copy_from_slice(bytes);
        }
    }

    fn model(resolve_end: usize, locked_end: usize) -> Model {
        Model {
            bytes: RefCell::new([0xa5; 16]),
            resolve_end,
            locked_end,
        }
    }

    #[test]
    fn information_publishes_record_and_size() {
        let memory = model(16, 16);
        information(&memory, 0, 4, &[1, 2, 3, 4], 8).unwrap();
        assert_eq!(&memory.bytes.borrow()[..4], &[1, 2, 3, 4]);
        assert_eq!(&memory.bytes.borrow()[8..], &4u64.to_ne_bytes());
    }

    #[test]
    fn short_information_only_publishes_size_and_ignores_buffer() {
        let memory = model(16, 16);
        assert_eq!(
            information(&memory, u64::MAX, 0, &[1, 2], 8),
            Err(Error::BufferTooSmall)
        );
        assert_eq!(&memory.bytes.borrow()[..8], &[0xa5; 8]);
        assert_eq!(&memory.bytes.borrow()[8..], &2u64.to_ne_bytes());
    }

    #[test]
    fn information_length_fault_does_not_publish_record_or_partial_size() {
        for capacity in [0, 4] {
            let memory = model(12, 16);
            assert_eq!(
                information(&memory, 0, capacity, &[1, 2, 3, 4], 8),
                Err(Error::InvalidArgs)
            );
            assert_eq!(*memory.bytes.borrow(), [0xa5; 16]);
        }
    }

    #[test]
    fn later_output_failure_leaves_every_output_untouched() {
        let memory = model(8, 16);
        assert_eq!(
            write(&memory, &[(0, &[1, 2]), (8, &[3])]),
            Err(Error::InvalidArgs)
        );
        assert_eq!(*memory.bytes.borrow(), [0xa5; 16]);
    }

    #[test]
    fn crossing_inaccessible_backing_does_not_write_a_prefix() {
        let memory = model(8, 16);
        assert_eq!(
            write(&memory, &[(6, &[1, 2, 3, 4])]),
            Err(Error::InvalidArgs)
        );
        assert_eq!(*memory.bytes.borrow(), [0xa5; 16]);
    }

    #[test]
    fn rechecks_all_outputs_under_the_lock() {
        let memory = model(16, 8);
        assert_eq!(
            write(&memory, &[(0, &[1]), (8, &[2])]),
            Err(Error::InvalidArgs)
        );
        assert_eq!(*memory.bytes.borrow(), [0xa5; 16]);
    }

    #[test]
    fn writes_overlapping_outputs_in_order() {
        let memory = model(16, 16);
        write(&memory, &[(0, &[1, 2, 3]), (1, &[4, 5])]).unwrap();
        assert_eq!(&memory.bytes.borrow()[..4], &[1, 4, 5, 0xa5]);
    }

    #[test]
    fn ignores_empty_outputs_and_rejects_overflow() {
        let memory = model(16, 16);
        write(&memory, &[(u64::MAX, &[])]).unwrap();
        assert_eq!(write(&memory, &[(u64::MAX, &[1])]), Err(Error::InvalidArgs));
        assert_eq!(*memory.bytes.borrow(), [0xa5; 16]);
    }
}
