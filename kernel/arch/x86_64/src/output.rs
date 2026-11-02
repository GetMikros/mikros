//! Preparation and publication of bounded batches of syscall outputs.

use kernel_core::object::budget::Budget;
use kernel_core::object::channel::HANDLES_MAX;
use kernel_core::object::handles::{Entry, HandleTable};
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

/// Publish both new endpoint handles as one output batch.
pub fn channel_pair(
    memory: &impl Memory,
    outputs: [u64; 2],
    handles: [u32; 2],
) -> Result<(), Error> {
    write(
        memory,
        &[
            (outputs[0], &handles[0].to_ne_bytes()),
            (outputs[1], &handles[1].to_ne_bytes()),
        ],
    )
}

/// Publish the two message sizes together, including on size-reporting errors.
pub fn channel_sizes(
    memory: &impl Memory,
    outputs: [u64; 2],
    len: u64,
    count: u64,
) -> Result<(), Error> {
    write(
        memory,
        &[
            (outputs[0], &len.to_ne_bytes()),
            (outputs[1], &count.to_ne_bytes()),
        ],
    )
}

/// Publish a received message and its sizes in the original output order.
pub fn channel_message(
    memory: &impl Memory,
    outputs: [u64; 4],
    bytes: &[u8],
    handles: &[u32],
) -> Result<(), Error> {
    // SAFETY: u32 has no padding and the slice covers exactly these handles.
    let values = unsafe {
        core::slice::from_raw_parts(
            handles.as_ptr().cast::<u8>(),
            core::mem::size_of_val(handles),
        )
    };
    write(
        memory,
        &[
            (outputs[0], bytes),
            (outputs[1], values),
            (outputs[2], &(bytes.len() as u64).to_ne_bytes()),
            (outputs[3], &(handles.len() as u64).to_ne_bytes()),
        ],
    )
}

/// Install received handles, keeping them only after all outputs publish.
pub fn receive(
    memory: &impl Memory,
    outputs: [u64; 4],
    bytes: &[u8],
    entries: &[Entry],
    table: &mut HandleTable,
    budget: Option<&alloc::sync::Arc<Budget>>,
) -> Result<(), Error> {
    let mut values = [0; HANDLES_MAX];
    let mut installed = 0;
    let mut result = Ok(());
    for entry in entries {
        match table.insert(entry.object.clone(), entry.rights, budget) {
            Ok(handle) => {
                values[installed] = handle;
                installed += 1;
            }
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }
    let values = &values[..installed];
    result = result.and_then(|()| channel_message(memory, outputs, bytes, values));
    if let Err(e) = result {
        for &handle in values {
            table.close(handle).expect("just inserted");
        }
        if e == Error::NoResources {
            channel_sizes(
                memory,
                [outputs[2], outputs[3]],
                bytes.len() as u64,
                entries.len() as u64,
            )?;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_core::object::channel::{Endpoint, Message};
    use kernel_core::object::handles::SLOT_COST;
    use kernel_core::object::rights;
    use std::cell::{RefCell, RefMut};
    use std::sync::Arc;

    struct Model {
        bytes: RefCell<Vec<u8>>,
        resolve_end: usize,
        locked_end: usize,
    }

    struct Guard<'a> {
        bytes: RefMut<'a, Vec<u8>>,
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
            bytes: RefCell::new(vec![0xa5; 16.max(resolve_end).max(locked_end)]),
            resolve_end,
            locked_end,
        }
    }

    fn entries() -> Vec<Entry> {
        let (a, b) = Endpoint::pair(None).unwrap();
        vec![
            Entry {
                object: a,
                rights: rights::READ,
            },
            Entry {
                object: b,
                rights: rights::WRITE,
            },
        ]
    }

    #[test]
    fn channel_pair_second_fault_is_atomic_and_success_publishes_both() {
        for (resolve_end, locked_end) in [(6, 16), (16, 6)] {
            let memory = model(resolve_end, locked_end);
            assert_eq!(
                channel_pair(&memory, [0, 4], [7, 9]),
                Err(Error::InvalidArgs)
            );
            assert_eq!(*memory.bytes.borrow(), [0xa5; 16]);
        }
        let memory = model(16, 16);
        channel_pair(&memory, [0, 4], [7, 9]).unwrap();
        assert_eq!(&memory.bytes.borrow()[..4], &7u32.to_ne_bytes());
        assert_eq!(&memory.bytes.borrow()[4..8], &9u32.to_ne_bytes());
        assert_eq!(&memory.bytes.borrow()[8..], &[0xa5; 8]);
    }

    #[test]
    fn channel_sizes_second_crossing_fault_leaves_both_untouched() {
        for (resolve_end, locked_end) in [(12, 16), (16, 12)] {
            let memory = model(resolve_end, locked_end);
            assert_eq!(
                channel_sizes(&memory, [0, 8], 3, 2),
                Err(Error::InvalidArgs)
            );
            assert_eq!(*memory.bytes.borrow(), [0xa5; 16]);
        }
    }

    #[test]
    fn channel_message_checks_each_output_before_publishing() {
        for (resolve_end, locked_end) in [(32, 64), (64, 32)] {
            for i in 0..4 {
                let memory = model(resolve_end, locked_end);
                let mut outputs = [0, 4, 16, 24];
                outputs[i] = 31;
                assert_eq!(
                    channel_message(&memory, outputs, &[1, 2], &[7, 9]),
                    Err(Error::InvalidArgs)
                );
                assert_eq!(*memory.bytes.borrow(), [0xa5; 64]);
            }
        }
        let memory = model(32, 32);
        channel_message(&memory, [0, 4, 16, 24], &[1, 2], &[7, 9]).unwrap();
        let mut expected = vec![0xa5; 32];
        expected[..2].copy_from_slice(&[1, 2]);
        expected[4..8].copy_from_slice(&7u32.to_ne_bytes());
        expected[8..12].copy_from_slice(&9u32.to_ne_bytes());
        expected[16..24].copy_from_slice(&2u64.to_ne_bytes());
        expected[24..32].copy_from_slice(&2u64.to_ne_bytes());
        assert_eq!(*memory.bytes.borrow(), expected);
    }

    #[test]
    fn receive_publication_fault_rolls_back_and_retry_consumes_message() {
        for (resolve_end, locked_end) in [(32, 64), (64, 32)] {
            for i in 0..4 {
                let (sender, receiver) = Endpoint::pair(None).unwrap();
                let entries = entries();
                let message = Message::new(vec![1, 2], entries.clone(), None).unwrap();
                assert!(sender.write(message).is_ok());
                let mut table = HandleTable::new();
                let memory = model(resolve_end, locked_end);
                let mut outputs = [0, 4, 16, 24];
                outputs[i] = 31;
                assert_eq!(
                    receiver.read(|bytes, entries| {
                        receive(&memory, outputs, bytes, entries, &mut table, None)
                    }),
                    Err(Error::InvalidArgs)
                );
                assert!(table.is_empty());
                assert_eq!(*memory.bytes.borrow(), [0xa5; 64]);
                let memory = model(32, 32);
                receiver
                    .read(|bytes, queued| {
                        assert_eq!(bytes, &[1, 2]);
                        assert_eq!(queued.len(), entries.len());
                        receive(&memory, [0, 4, 16, 24], bytes, queued, &mut table, None)
                    })
                    .unwrap();
                assert_eq!(table.len(), 2);
                let bytes = memory.bytes.borrow();
                assert_eq!(&bytes[..2], &[1, 2]);
                for (i, original) in entries.iter().enumerate() {
                    let start = 4 + i * 4;
                    let handle = u32::from_ne_bytes(bytes[start..start + 4].try_into().unwrap());
                    let installed = table.get(handle).unwrap();
                    assert!(Arc::ptr_eq(&installed.object, &original.object));
                    assert_eq!(installed.rights, original.rights);
                }
                assert_eq!(&bytes[16..24], &2u64.to_ne_bytes());
                assert_eq!(&bytes[24..32], &2u64.to_ne_bytes());
                assert_eq!(
                    receiver.read(|_, _| Ok::<(), Error>(())),
                    Err(Error::ShouldWait)
                );
            }
        }
    }

    #[test]
    fn receive_partial_install_exhaustion_only_publishes_sizes_unless_faulted() {
        for (resolve_end, locked_end) in [(32, 32), (28, 32), (32, 28)] {
            let root = Budget::root();
            let budget = Budget::child(&root, SLOT_COST, u64::MAX, None).unwrap();
            let mut table = HandleTable::new();
            let memory = model(resolve_end, locked_end);
            let result = receive(
                &memory,
                [u64::MAX, u64::MAX, 16, 24],
                &[1, 2, 3],
                &entries(),
                &mut table,
                Some(&budget),
            );
            assert!(table.is_empty());
            let mut expected = vec![0xa5; 32];
            if resolve_end == 32 && locked_end == 32 {
                assert_eq!(result, Err(Error::NoResources));
                expected[16..24].copy_from_slice(&3u64.to_ne_bytes());
                expected[24..32].copy_from_slice(&2u64.to_ne_bytes());
            } else {
                assert_eq!(result, Err(Error::InvalidArgs));
            }
            assert_eq!(*memory.bytes.borrow(), expected);
        }
    }

    #[test]
    fn channel_message_publishes_large_payload_and_handle_array() {
        let memory = model(65808, 65808);
        let payload = vec![0x5a; 65536];
        let handles: Vec<u32> = (1..=64).collect();
        channel_message(&memory, [0, 65536, 65792, 65800], &payload, &handles).unwrap();
        let bytes = memory.bytes.borrow();
        assert_eq!(&bytes[..65536], payload.as_slice());
        for (chunk, handle) in bytes[65536..65792].chunks_exact(4).zip(handles) {
            assert_eq!(chunk, &handle.to_ne_bytes());
        }
        assert_eq!(&bytes[65792..65800], &65536u64.to_ne_bytes());
        assert_eq!(&bytes[65800..], &64u64.to_ne_bytes());
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
