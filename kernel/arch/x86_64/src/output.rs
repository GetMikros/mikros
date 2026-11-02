//! Preparation and publication of bounded batches of syscall outputs.

use crate::publish::{Outputs, Page, Work};
use alloc::sync::Arc;
use kernel_core::object::budget::Budget;
use kernel_core::object::channel::HANDLES_MAX;
use kernel_core::object::handles::{Entry, HANDLE_INVALID, Handle, HandleTable};
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

/// Installs a received message's handles once `prepare` has prepared its
/// outputs, keeping both only when both succeed. When the table cannot
/// hold the handles, publishes the two sizes with `sizes`.
pub fn accept<P: Page, F: FnMut()>(
    entries: &[Entry],
    table: &mut HandleTable,
    budget: Option<&Arc<Budget>>,
    work: &mut Work<F>,
    prepare: impl FnOnce(&mut Work<F>) -> Result<Outputs<P>, Error>,
    sizes: impl FnOnce() -> Result<(), Error>,
) -> Result<(Outputs<P>, [Handle; HANDLES_MAX]), Error> {
    let outputs = prepare(work)?;
    let mut values = [HANDLE_INVALID; HANDLES_MAX];
    let installed = table.insert_many_then::<HANDLES_MAX>(entries, budget, |handles| {
        values[..handles.len()].copy_from_slice(handles);
        Ok(())
    });
    match installed {
        Ok(()) => Ok((outputs, values)),
        Err(e) => {
            outputs.release(work);
            if e == Error::NoResources {
                sizes()?;
            }
            Err(e)
        }
    }
}

/// Writes a received message's bytes, its installed handles and their two
/// sizes, in that order, to the outputs `accept` prepared, and releases
/// them.
pub fn deliver<P: Page, F: FnMut()>(
    outputs: Outputs<P>,
    bytes: &[u8],
    handles: &[Handle],
    work: &mut Work<F>,
) {
    let mut values = [0u8; HANDLES_MAX * size_of::<Handle>()];
    let (slots, _) = values.as_chunks_mut::<{ size_of::<Handle>() }>();
    for (value, handle) in slots.iter_mut().zip(handles) {
        value.copy_from_slice(&handle.to_ne_bytes());
    }
    outputs.write(0, 0, bytes, work);
    outputs.write(1, 0, &values[..size_of_val(handles)], work);
    outputs.write(2, 0, &(bytes.len() as u64).to_ne_bytes(), work);
    outputs.write(3, 0, &(handles.len() as u64).to_ne_bytes(), work);
    outputs.release(work);
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

    /// User memory of `SIZE` bytes from address 0 whose pages below
    /// `limit` can be prepared.
    struct User {
        bytes: Vec<u8>,
        limit: u64,
    }

    const SIZE: usize = 0x30000;
    const PAGE: u64 = kernel_core::object::memory::PAGE_SIZE;

    struct UserPage(*mut u8);

    impl Page for UserPage {
        fn base(&self) -> *mut u8 {
            self.0
        }
    }

    impl User {
        fn new(limit: u64) -> User {
            User {
                bytes: vec![0xa5; SIZE],
                limit,
            }
        }

        fn prepare<F: FnMut()>(
            &mut self,
            ranges: &[(u64, u64)],
            work: &mut Work<F>,
        ) -> Result<Outputs<UserPage>, Error> {
            let (base, limit) = (self.bytes.as_mut_ptr(), self.limit);
            Outputs::prepare(ranges, None, work, |page| {
                if page + PAGE > limit {
                    return Err(Error::InvalidArgs);
                }
                // SAFETY: the page lies inside the buffer.
                Ok(UserPage(unsafe { base.add(page as usize) }))
            })
        }
    }

    fn ranges(outputs: [u64; 4], len: usize, count: usize) -> [(u64, u64); 4] {
        [
            (outputs[0], len as u64),
            (outputs[1], (count * 4) as u64),
            (outputs[2], 8),
            (outputs[3], 8),
        ]
    }

    /// Reads one message from `receiver` into `user` as channel_read does.
    fn receive(
        receiver: &Endpoint,
        user: &mut User,
        outputs: [u64; 4],
        table: &mut HandleTable,
        budget: Option<&Arc<Budget>>,
        checks: &mut usize,
    ) -> Result<(), Error> {
        let sizes = RefCell::new(None);
        let mut work = Work::new(|| panic!("no check while locked"));
        let taken = receiver.take(|data, entries| {
            let ranges = ranges(outputs, data.len(), entries.len());
            let prepared = accept(
                entries,
                table,
                budget,
                &mut work,
                |work| user.prepare(&ranges, work),
                || {
                    *sizes.borrow_mut() = Some((data.len(), entries.len()));
                    Ok(())
                },
            );
            prepared.map(|(prepared, values)| (prepared, values, entries.len()))
        });
        if let Some((len, count)) = sizes.take() {
            user.bytes[outputs[2] as usize..][..8].copy_from_slice(&(len as u64).to_ne_bytes());
            user.bytes[outputs[3] as usize..][..8].copy_from_slice(&(count as u64).to_ne_bytes());
        }
        let ((prepared, values, count), message) = taken?;
        let mut work = Work::new(|| *checks += 1);
        deliver(prepared, &message.bytes, &values[..count], &mut work);
        Ok(())
    }

    #[test]
    fn unprepared_message_output_leaves_message_and_table() {
        for i in 0..4 {
            let (sender, receiver) = Endpoint::pair(None).unwrap();
            let entries = entries();
            assert!(
                sender
                    .write(Message::new(vec![1, 2], entries.clone(), None).unwrap())
                    .is_ok()
            );
            let mut table = HandleTable::new();
            let mut user = User::new(0x2000);
            let mut outputs = [0x10, 0x20, 0x30, 0x38];
            outputs[i] = 0x1fff;
            let mut checks = 0;
            assert_eq!(
                receive(&receiver, &mut user, outputs, &mut table, None, &mut checks),
                Err(Error::InvalidArgs)
            );
            assert!(table.is_empty());
            assert!(user.bytes.iter().all(|&b| b == 0xa5));
            let outputs = [0x10, 0x20, 0x30, 0x38];
            receive(&receiver, &mut user, outputs, &mut table, None, &mut checks).unwrap();
            assert_eq!(table.len(), 2);
            assert_eq!(&user.bytes[0x10..0x12], &[1, 2]);
            for (i, original) in entries.iter().enumerate() {
                let at = 0x20 + i * 4;
                let handle = u32::from_ne_bytes(user.bytes[at..at + 4].try_into().unwrap());
                assert!(Arc::ptr_eq(
                    &table.get(handle).unwrap().object,
                    &original.object
                ));
            }
            assert_eq!(&user.bytes[0x30..0x38], &2u64.to_ne_bytes());
            assert_eq!(&user.bytes[0x38..0x40], &2u64.to_ne_bytes());
            assert_eq!(
                receiver.read(|_, _| Ok::<(), Error>(())),
                Err(Error::ShouldWait)
            );
        }
    }

    #[test]
    fn full_table_only_publishes_sizes_and_keeps_message() {
        let root = Budget::root();
        let budget = Budget::child(&root, SLOT_COST, u64::MAX, None).unwrap();
        let (sender, receiver) = Endpoint::pair(None).unwrap();
        assert!(
            sender
                .write(Message::new(vec![1, 2, 3], entries(), None).unwrap())
                .is_ok()
        );
        let mut table = HandleTable::new();
        let mut user = User::new(0x2000);
        let mut checks = 0;
        assert_eq!(
            receive(
                &receiver,
                &mut user,
                [0x10, 0x20, 0x30, 0x38],
                &mut table,
                Some(&budget),
                &mut checks
            ),
            Err(Error::NoResources)
        );
        assert!(table.is_empty());
        let mut expected = vec![0xa5; SIZE];
        expected[0x30..0x38].copy_from_slice(&3u64.to_ne_bytes());
        expected[0x38..0x40].copy_from_slice(&2u64.to_ne_bytes());
        assert!(user.bytes == expected);
        assert!(receiver.read(|_, _| Ok::<(), Error>(())).is_ok());
    }

    #[test]
    fn largest_message_is_delivered_with_one_check() {
        let (sender, receiver) = Endpoint::pair(None).unwrap();
        let payload: Vec<u8> = (0..65536u32).map(|i| i as u8).collect();
        let handles: Vec<Entry> = (0..32).flat_map(|_| entries()).collect();
        assert!(
            sender
                .write(Message::new(payload.clone(), handles, None).unwrap())
                .is_ok()
        );
        let mut table = HandleTable::new();
        let mut user = User::new(SIZE as u64);
        let mut checks = 0;
        let outputs = [0x123, 0x10200, 0x10300, 0x10308];
        receive(&receiver, &mut user, outputs, &mut table, None, &mut checks).unwrap();
        assert_eq!(checks, 1);
        assert_eq!(table.len(), 64);
        assert_eq!(&user.bytes[0x123..0x123 + 65536], payload.as_slice());
        assert_eq!(&user.bytes[0x10300..0x10308], &65536u64.to_ne_bytes());
        assert_eq!(&user.bytes[0x10308..0x10310], &64u64.to_ne_bytes());
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
