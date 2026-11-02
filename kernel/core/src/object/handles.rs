//! Handle tables.

use super::{Object, ObjectType, rights};
use crate::status::Error;
use alloc::sync::Arc;
use alloc::vec::Vec;

pub type Handle = u32;
pub const HANDLE_INVALID: Handle = 0;
/// The most handles one table holds.
pub const HANDLE_LIMIT: usize = 65536;

const GENERATION_BITS: u32 = 8;
const GENERATION_MASK: u32 = (1 << GENERATION_BITS) - 1;

/// What a handle refers to.
#[derive(Clone)]
pub struct Entry {
    pub object: Arc<dyn Object>,
    pub rights: u32,
}

struct Slot {
    /// Incremented each time the slot is freed.
    generation: u32,
    entry: Option<Entry>,
}

/// The handles of one process.
#[derive(Default)]
pub struct HandleTable {
    slots: Vec<Slot>,
    /// Indices of free slots.
    free: Vec<u32>,
    count: usize,
}

fn value(index: usize, generation: u32) -> Handle {
    ((index as u32 + 1) << GENERATION_BITS) | generation
}

impl HandleTable {
    pub const fn new() -> Self {
        HandleTable {
            slots: Vec::new(),
            free: Vec::new(),
            count: 0,
        }
    }

    /// The number of handles in the table.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Adds a handle to `object` with `rights`.
    pub fn insert(&mut self, object: Arc<dyn Object>, rights: u32) -> Result<Handle, Error> {
        if self.count >= HANDLE_LIMIT {
            return Err(Error::NoResources);
        }
        let index = match self.free.pop() {
            Some(index) => index as usize,
            None => {
                // The free list gets room for every slot now, so that
                // closing a handle never needs memory.
                self.slots.try_reserve(1).map_err(|_| Error::NoMemory)?;
                self.free
                    .try_reserve(self.slots.len() + 1 - self.free.len())
                    .map_err(|_| Error::NoMemory)?;
                self.slots.push(Slot {
                    generation: 0,
                    entry: None,
                });
                self.slots.len() - 1
            }
        };
        let slot = &mut self.slots[index];
        slot.entry = Some(Entry { object, rights });
        self.count += 1;
        Ok(value(index, slot.generation))
    }

    fn index(&self, handle: Handle) -> Result<usize, Error> {
        let index = (handle >> GENERATION_BITS) as usize;
        let slot = index
            .checked_sub(1)
            .and_then(|i| self.slots.get(i).map(|s| (i, s)));
        match slot {
            Some((i, s)) if s.entry.is_some() && s.generation == handle & GENERATION_MASK => Ok(i),
            _ => Err(Error::BadHandle),
        }
    }

    /// The entry of `handle`.
    pub fn get(&self, handle: Handle) -> Result<&Entry, Error> {
        let i = self.index(handle)?;
        self.slots[i].entry.as_ref().ok_or(Error::BadHandle)
    }

    /// The entry of `handle`, after checking in this order that the handle
    /// is valid, that its object has one of the `accepted` types (any type
    /// when empty), and that it holds every right in `required`.
    pub fn check(
        &self,
        handle: Handle,
        accepted: &[ObjectType],
        required: u32,
    ) -> Result<&Entry, Error> {
        let entry = self.get(handle)?;
        let kind = entry.object.header().kind();
        if !accepted.is_empty() && !accepted.contains(&kind) {
            return Err(Error::WrongType);
        }
        if entry.rights & required != required {
            return Err(Error::AccessDenied);
        }
        Ok(entry)
    }

    /// Removes `handle`. Removing `HANDLE_INVALID` does nothing.
    pub fn close(&mut self, handle: Handle) -> Result<(), Error> {
        if handle == HANDLE_INVALID {
            return Ok(());
        }
        self.take(handle).map(drop)
    }

    fn take(&mut self, handle: Handle) -> Result<Entry, Error> {
        let i = self.index(handle)?;
        let slot = &mut self.slots[i];
        let entry = slot.entry.take().ok_or(Error::BadHandle)?;
        slot.generation = (slot.generation + 1) & GENERATION_MASK;
        // Room was reserved when the slot was added.
        self.free.push(i as u32);
        self.count -= 1;
        Ok(entry)
    }

    /// `rights`, with `SAME_RIGHTS` standing for `source`, if they are a
    /// subset of `source`.
    fn derived(source: u32, rights: u32) -> Result<u32, Error> {
        let rights = if rights == rights::SAME_RIGHTS {
            source
        } else {
            rights
        };
        if rights & !source != 0 {
            return Err(Error::InvalidArgs);
        }
        Ok(rights)
    }

    /// A new handle to the object of `handle` with `rights`. Requires the
    /// `DUPLICATE` right.
    pub fn duplicate(&mut self, handle: Handle, rights: u32) -> Result<Handle, Error> {
        let entry = self.check(handle, &[], rights::DUPLICATE)?;
        let rights = Self::derived(entry.rights, rights)?;
        let object = entry.object.clone();
        self.insert(object, rights)
    }

    /// Replaces `handle` with a new handle to its object with `rights`.
    /// `handle` stays open when `rights` is not a subset of its rights.
    pub fn replace(&mut self, handle: Handle, rights: u32) -> Result<Handle, Error> {
        let rights = Self::derived(self.get(handle)?.rights, rights)?;
        let entry = self.take(handle)?;
        // The slot just freed makes room, and inserting into a free slot
        // allocates nothing.
        self.insert(entry.object, rights)
    }

    /// Like `duplicate`, but passes the new handle to `publish` and keeps
    /// it only when `publish` succeeds; otherwise the table is left as it
    /// was and the error is returned.
    pub fn duplicate_then(
        &mut self,
        handle: Handle,
        rights: u32,
        publish: impl FnOnce(Handle) -> Result<(), Error>,
    ) -> Result<Handle, Error> {
        let new = self.duplicate(handle, rights)?;
        if let Err(e) = publish(new) {
            self.take(new).map(drop)?;
            return Err(e);
        }
        Ok(new)
    }

    /// Like `replace`, but first passes the value the new handle will
    /// have to `publish`, and changes nothing when it fails.
    pub fn replace_then(
        &mut self,
        handle: Handle,
        rights: u32,
        publish: impl FnOnce(Handle) -> Result<(), Error>,
    ) -> Result<Handle, Error> {
        let rights = Self::derived(self.get(handle)?.rights, rights)?;
        // `take` frees the slot and `insert` takes the slot freed last,
        // so the new handle has the next generation of the same slot.
        let i = self.index(handle)?;
        let next = value(i, (self.slots[i].generation + 1) & GENERATION_MASK);
        publish(next)?;
        let entry = self.take(handle)?;
        let new = self.insert(entry.object, rights)?;
        debug_assert_eq!(new, next);
        Ok(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::tests::Plain;
    use crate::object::{Header, default_rights};

    fn event() -> Arc<dyn Object> {
        Arc::new(Plain(Header::new(ObjectType::Event)))
    }

    #[test]
    fn publishes_new_handles_before_keeping_them() {
        let mut t = HandleTable::new();
        let a = t.insert(event(), rights::DUPLICATE | rights::READ).unwrap();
        let fail = |_| Err(Error::InvalidArgs);
        assert_eq!(
            t.duplicate_then(a, rights::READ, fail),
            Err(Error::InvalidArgs)
        );
        assert_eq!(t.len(), 1);
        let mut seen = 0;
        let d = t.duplicate_then(a, rights::READ, |h| {
            seen = h;
            Ok(())
        });
        assert_eq!(d, Ok(seen));
        assert_eq!(t.get(seen).unwrap().rights, rights::READ);

        assert_eq!(
            t.replace_then(a, rights::READ, fail),
            Err(Error::InvalidArgs)
        );
        assert!(t.get(a).is_ok());
        assert_eq!(
            t.replace_then(a, rights::WRITE, |_| Ok(())),
            Err(Error::InvalidArgs)
        );
        let r = t.replace_then(a, rights::READ, |h| {
            seen = h;
            Ok(())
        });
        assert_eq!(r, Ok(seen));
        assert!(t.get(a).is_err());
        assert_eq!(t.get(seen).unwrap().rights, rights::READ);
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn never_reissues_a_closed_value_at_once() {
        let mut t = HandleTable::new();
        let a = t.insert(event(), 0).unwrap();
        assert_ne!(a, HANDLE_INVALID);
        t.close(a).unwrap();
        assert_eq!(t.get(a).err(), Some(Error::BadHandle));
        let b = t.insert(event(), 0).unwrap();
        assert_eq!(b >> 8, a >> 8);
        assert_ne!(b, a);
        assert_eq!(t.close(a), Err(Error::BadHandle));
        assert_eq!(t.close(HANDLE_INVALID), Ok(()));
        assert_eq!(t.get(HANDLE_INVALID).err(), Some(Error::BadHandle));
        assert_eq!(t.get(0xFFFF_FF00).err(), Some(Error::BadHandle));
    }

    #[test]
    fn generations_wrap_after_256_closes() {
        let mut t = HandleTable::new();
        let first = t.insert(event(), 0).unwrap();
        let mut h = first;
        for _ in 0..256 {
            t.close(h).unwrap();
            h = t.insert(event(), 0).unwrap();
        }
        assert_eq!(h, first);
    }

    #[test]
    fn checks_validity_type_and_rights_in_order() {
        let mut t = HandleTable::new();
        let h = t.insert(event(), rights::WAIT).unwrap();
        let timer = [ObjectType::Timer];
        assert_eq!(
            t.check(h + 1, &timer, rights::SIGNAL).err(),
            Some(Error::BadHandle)
        );
        assert_eq!(
            t.check(h, &timer, rights::SIGNAL).err(),
            Some(Error::WrongType)
        );
        assert_eq!(
            t.check(h, &[ObjectType::Event], rights::SIGNAL).err(),
            Some(Error::AccessDenied)
        );
        assert!(t.check(h, &[ObjectType::Event], rights::WAIT).is_ok());
    }

    #[test]
    fn duplicates_and_replaces_with_fewer_rights() {
        let mut t = HandleTable::new();
        let all = default_rights(ObjectType::Event);
        let h = t.insert(event(), all).unwrap();
        let koid = t.get(h).unwrap().object.header().koid();

        let d = t.duplicate(h, rights::SAME_RIGHTS).unwrap();
        assert_eq!(t.get(d).unwrap().rights, all);
        assert_eq!(t.get(d).unwrap().object.header().koid(), koid);
        assert_eq!(t.duplicate(h, rights::MAP), Err(Error::InvalidArgs));

        let r = t.replace(d, rights::WAIT).unwrap();
        assert_eq!(t.get(d).err(), Some(Error::BadHandle));
        assert_eq!(t.get(r).unwrap().rights, rights::WAIT);
        // Without DUPLICATE, duplicating is denied but replacing works.
        assert_eq!(t.duplicate(r, rights::WAIT), Err(Error::AccessDenied));
        assert_eq!(t.replace(r, rights::SIGNAL), Err(Error::InvalidArgs));
        assert!(t.get(r).is_ok());
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn enforces_the_limit_and_frees_objects() {
        let mut t = HandleTable::new();
        let object = event();
        let handles: Vec<Handle> = (0..HANDLE_LIMIT)
            .map(|_| t.insert(object.clone(), rights::DUPLICATE).unwrap())
            .collect();
        assert_eq!(t.insert(object.clone(), 0), Err(Error::NoResources));
        assert_eq!(
            t.duplicate(handles[0], rights::SAME_RIGHTS),
            Err(Error::NoResources)
        );
        // Replacing works on a full table.
        let r = t.replace(handles[0], 0).unwrap();
        assert_eq!(Arc::strong_count(&object), HANDLE_LIMIT + 1);
        t.close(r).unwrap();
        for &h in &handles[1..] {
            t.close(h).unwrap();
        }
        assert!(t.is_empty());
        assert_eq!(Arc::strong_count(&object), 1);
    }
}
