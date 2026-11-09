//! Installing the handles of received channel messages and publishing
//! the messages to their prepared outputs.

use crate::object::budget::Budget;
use crate::object::channel::HANDLES_MAX;
use crate::object::handles::{Entry, HANDLE_INVALID, Handle, HandleTable};
use crate::publish::{Outputs, Page, Work};
use crate::status::Error;
use alloc::sync::Arc;

/// Installs a received message's handles in `table`, charged to `budget`,
/// keeping none unless all fit, and returns them.
pub fn install(
    entries: &[Entry],
    table: &mut HandleTable,
    budget: Option<&Arc<Budget>>,
) -> Result<[Handle; HANDLES_MAX], Error> {
    let mut values = [HANDLE_INVALID; HANDLES_MAX];
    table.insert_many_then::<HANDLES_MAX>(entries, budget, |handles| {
        values[..handles.len()].copy_from_slice(handles);
        Ok(())
    })?;
    Ok(values)
}

/// Writes only the two sizes of a message of `len` bytes and `count`
/// handles to the outputs prepared for it, as when the table cannot hold
/// its handles, and releases them.
pub fn deliver_sizes<P: Page, F: FnMut()>(
    outputs: Outputs<P>,
    len: u64,
    count: u64,
    work: &mut Work<F>,
) {
    outputs.write(2, 0, &len.to_ne_bytes(), work);
    outputs.write(3, 0, &count.to_ne_bytes(), work);
    outputs.release(work);
}

/// Writes a received message's bytes, its installed handles and their two
/// sizes, in that order, to the outputs prepared for it, and releases
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
    use crate::object::channel::{Endpoint, Message};
    use crate::object::handles::SLOT_COST;
    use crate::object::rights;
    use std::sync::Arc;

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
    const PAGE: u64 = crate::object::memory::PAGE_SIZE;

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

    /// Reads one message from `receiver` into `user` as channel_read does:
    /// prepares the outputs for the oldest message's sizes, then installs
    /// its handles and takes it.
    fn receive(
        receiver: &Endpoint,
        user: &mut User,
        outputs: [u64; 4],
        table: &mut HandleTable,
        budget: Option<&Arc<Budget>>,
        checks: &mut usize,
    ) -> Result<(), Error> {
        let (len, count) = receiver.front_sizes()?;
        let ranges = ranges(outputs, len as usize, count as usize);
        let prepared = user.prepare(&ranges, &mut Work::new(|| {}))?;
        let taken = receiver.take(|_, entries| install(entries, table, budget));
        let mut work = Work::new(|| *checks += 1);
        match taken {
            Ok((values, message)) => {
                deliver(
                    prepared,
                    &message.bytes,
                    &values[..count as usize],
                    &mut work,
                );
                Ok(())
            }
            Err(Error::NoResources) => {
                deliver_sizes(prepared, len, count, &mut work);
                Err(Error::NoResources)
            }
            Err(e) => {
                prepared.release(&mut work);
                Err(e)
            }
        }
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
}
