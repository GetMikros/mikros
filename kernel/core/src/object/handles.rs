//! Handle tables.

use super::budget::Budget;
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
    /// The budget charged `SLOT_COST` when the slot was added.
    charged: Option<Arc<Budget>>,
}

/// The kernel memory charged for adding a slot: the slot and its place in
/// the free list.
pub const SLOT_COST: u64 = (size_of::<Slot>() + size_of::<u32>()) as u64;

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(budget) = &self.charged {
            budget.release_kernel(SLOT_COST);
        }
    }
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

    /// Adds a handle to `object` with `rights`. When no free slot is left
    /// and a new one is added, `SLOT_COST` is charged to `budget`, if
    /// given, and released to it when the table is dropped.
    pub fn insert(
        &mut self,
        object: Arc<dyn Object>,
        rights: u32,
        budget: Option<&Arc<Budget>>,
    ) -> Result<Handle, Error> {
        if self.count >= HANDLE_LIMIT {
            return Err(Error::NoResources);
        }
        let index = match self.free.pop() {
            Some(index) => index as usize,
            None => {
                if let Some(budget) = budget {
                    budget.charge_kernel(SLOT_COST)?;
                }
                let reserved = self.slots.try_reserve(1).is_ok()
                    // The free list gets room for every slot now, so that
                    // closing a handle never needs memory.
                    && self
                        .free
                        .try_reserve(self.slots.len() + 1 - self.free.len())
                        .is_ok();
                if !reserved {
                    if let Some(budget) = budget {
                        budget.release_kernel(SLOT_COST);
                    }
                    return Err(Error::NoMemory);
                }
                self.slots.push(Slot {
                    generation: 0,
                    entry: None,
                    charged: budget.cloned(),
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

    /// Removes `handle` and returns its entry.
    pub fn remove(&mut self, handle: Handle) -> Result<Entry, Error> {
        self.take(handle)
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
    /// `DUPLICATE` right. A new slot is charged to `budget` as in `insert`.
    pub fn duplicate(
        &mut self,
        handle: Handle,
        rights: u32,
        budget: Option<&Arc<Budget>>,
    ) -> Result<Handle, Error> {
        let entry = self.check(handle, &[], rights::DUPLICATE)?;
        let rights = Self::derived(entry.rights, rights)?;
        let object = entry.object.clone();
        self.insert(object, rights, budget)
    }

    /// Replaces `handle` with a new handle to its object with `rights`.
    /// `handle` stays open when `rights` is not a subset of its rights.
    pub fn replace(&mut self, handle: Handle, rights: u32) -> Result<Handle, Error> {
        let rights = Self::derived(self.get(handle)?.rights, rights)?;
        let entry = self.take(handle)?;
        // The slot just freed makes room, and inserting into a free slot
        // allocates nothing and charges nothing.
        self.insert(entry.object, rights, None)
    }

    /// Like `duplicate`, but passes the new handle to `publish` and keeps
    /// it only when `publish` succeeds; otherwise the table and the charges
    /// are left as they were and the error is returned.
    pub fn duplicate_then(
        &mut self,
        handle: Handle,
        rights: u32,
        budget: Option<&Arc<Budget>>,
        publish: impl FnOnce(Handle) -> Result<(), Error>,
    ) -> Result<Handle, Error> {
        let added = self.slots.len();
        let new = self.duplicate(handle, rights, budget)?;
        self.keep_if_published(new, added, publish)
    }

    /// Like `insert`, but passes the new handle to `publish` and keeps it
    /// only when `publish` succeeds; otherwise the table and the charges
    /// are left as they were and the error is returned.
    pub fn insert_then(
        &mut self,
        object: Arc<dyn Object>,
        rights: u32,
        budget: Option<&Arc<Budget>>,
        publish: impl FnOnce(Handle) -> Result<(), Error>,
    ) -> Result<Handle, Error> {
        let added = self.slots.len();
        let new = self.insert(object, rights, budget)?;
        self.keep_if_published(new, added, publish)
    }

    /// Inserts cloned entries and passes their handles, in order, to `publish`.
    /// On insertion or publication failure, removes the inserted handles and
    /// any slots added by this batch, releasing those slots' charges.
    pub fn insert_many_then<const N: usize>(
        &mut self,
        entries: &[Entry],
        budget: Option<&Arc<Budget>>,
        publish: impl FnOnce(&[Handle]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        assert!(entries.len() <= N);
        let added = self.slots.len();
        let mut handles = [HANDLE_INVALID; N];
        let mut inserted = 0;
        let result = (|| {
            for entry in entries {
                handles[inserted] = self.insert(entry.object.clone(), entry.rights, budget)?;
                inserted += 1;
            }
            publish(&handles[..inserted])
        })();
        if let Err(e) = result {
            for &handle in handles[..inserted].iter().rev() {
                let entry = self.take(handle).expect("just inserted");
                if self.slots.len() > added {
                    // Reverse insertion order puts the appended slot last in both lists.
                    self.free.pop();
                    self.slots.pop();
                }
                drop(entry);
            }
            return Err(e);
        }
        Ok(())
    }

    /// Passes `new` to `publish` and removes it again when that fails,
    /// together with its slot and the slot's charge if the slot was added
    /// at index `added`.
    fn keep_if_published(
        &mut self,
        new: Handle,
        added: usize,
        publish: impl FnOnce(Handle) -> Result<(), Error>,
    ) -> Result<Handle, Error> {
        let Err(e) = publish(new) else {
            return Ok(new);
        };
        let entry = self.take(new).expect("just inserted");
        if self.slots.len() > added {
            // `take` put the slot on the free list last.
            self.free.pop();
            self.slots.pop();
        }
        drop(entry);
        Err(e)
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
        let new = self.insert(entry.object, rights, None)?;
        debug_assert_eq!(new, next);
        Ok(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::budget::UNLIMITED;
    use crate::object::tests::Plain;
    use crate::object::{Header, default_rights};

    fn event() -> Arc<dyn Object> {
        Arc::new(Plain(Header::new(ObjectType::Event)))
    }

    #[test]
    fn publishes_new_handles_before_keeping_them() {
        let mut t = HandleTable::new();
        let a = t
            .insert(event(), rights::DUPLICATE | rights::READ, None)
            .unwrap();
        let fail = |_| Err(Error::InvalidArgs);
        assert_eq!(
            t.duplicate_then(a, rights::READ, None, fail),
            Err(Error::InvalidArgs)
        );
        assert_eq!(t.len(), 1);
        let mut seen = 0;
        let d = t.duplicate_then(a, rights::READ, None, |h| {
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

    fn kernel_used(budget: &Budget) -> u64 {
        u64::from_le_bytes(budget.info()[8..16].try_into().unwrap())
    }

    #[test]
    fn batch_insertion_budget_failure_rolls_back_partial_progress() {
        for reused in 0..=2 {
            let root = Budget::root();
            let owner = Budget::child(&root, UNLIMITED, UNLIMITED, None).unwrap();
            let small = Budget::child(&root, SLOT_COST, UNLIMITED, None).unwrap();
            let mut t = HandleTable::new();
            let live = t.insert(event(), rights::READ, Some(&owner)).unwrap();

            let mut old = [HANDLE_INVALID; 2];
            for h in &mut old[..reused] {
                *h = t.insert(event(), 0, Some(&owner)).unwrap();
            }
            for &h in &old[..reused] {
                t.close(h).unwrap();
            }
            let slots = t.slots.len();
            let free = t.free.clone();
            let count = t.len();
            let charged = kernel_used(&owner);
            let entries = core::array::from_fn::<_, 4, _>(|_| Entry {
                object: event(),
                rights: rights::READ,
            });
            for _ in 0..8 {
                assert_eq!(
                    t.insert_many_then::<4>(&entries, Some(&small), |_| {
                        panic!("partial insertion must not publish")
                    }),
                    Err(Error::NoResources)
                );
                assert_eq!((t.slots.len(), t.len()), (slots, count));
                assert_eq!(t.free, free);
                assert_eq!(kernel_used(&small), 0);
                assert_eq!(kernel_used(&owner), charged);
                assert_eq!(kernel_used(&root), charged);
                assert_eq!(t.get(live).unwrap().rights, rights::READ);
                for &h in &old[..reused] {
                    assert_eq!(t.get(h).err(), Some(Error::BadHandle));
                }
                for entry in &entries {
                    assert_eq!(Arc::strong_count(&entry.object), 1);
                }
            }
            drop(t);
            assert_eq!(kernel_used(&root), 0);
        }
    }

    #[test]
    fn batch_publication_failure_restores_slots_and_charges() {
        for reused in 0..=3 {
            let root = Budget::root();
            let owner = Budget::child(&root, UNLIMITED, UNLIMITED, None).unwrap();
            let batch = Budget::child(&root, UNLIMITED, UNLIMITED, None).unwrap();
            let mut t = HandleTable::new();
            let live = t.insert(event(), rights::WAIT, Some(&owner)).unwrap();
            let mut old = [HANDLE_INVALID; 3];
            for h in &mut old[..reused] {
                *h = t.insert(event(), 0, Some(&owner)).unwrap();
            }
            for &h in &old[..reused] {
                t.close(h).unwrap();
            }
            let free = t.free.clone();
            let slots = t.slots.len();
            let charged = kernel_used(&owner);
            let entries = core::array::from_fn::<_, 3, _>(|_| Entry {
                object: event(),
                rights: rights::READ,
            });
            for _ in 0..8 {
                let mut seen = [HANDLE_INVALID; 3];
                assert_eq!(
                    t.insert_many_then::<3>(&entries, Some(&batch), |handles| {
                        seen.copy_from_slice(handles);
                        assert_eq!(kernel_used(&batch), (3 - reused) as u64 * SLOT_COST);
                        Err(Error::InvalidArgs)
                    }),
                    Err(Error::InvalidArgs)
                );
                assert_eq!((t.slots.len(), t.len()), (slots, 1));
                assert_eq!(t.free, free);
                assert_eq!(kernel_used(&batch), 0);
                assert_eq!(kernel_used(&owner), charged);
                assert_eq!(kernel_used(&root), charged);
                assert_eq!(t.get(live).unwrap().rights, rights::WAIT);
                for h in seen {
                    assert_eq!(t.get(h).err(), Some(Error::BadHandle));
                }
                for &h in &old[..reused] {
                    assert_eq!(t.get(h).err(), Some(Error::BadHandle));
                }
                for entry in &entries {
                    assert_eq!(Arc::strong_count(&entry.object), 1);
                }
                for (&index, handle) in free.iter().rev().zip(seen) {
                    assert_ne!(
                        value(index as usize, t.slots[index as usize].generation),
                        handle
                    );
                }
            }
            drop(t);
            assert_eq!(kernel_used(&root), 0);
        }
    }

    #[test]
    fn batch_success_keeps_cloned_entries_in_order() {
        let root = Budget::root();
        let mut t = HandleTable::new();
        let live = t.insert(event(), rights::WAIT, Some(&root)).unwrap();
        let old = t.insert(event(), 0, Some(&root)).unwrap();
        t.close(old).unwrap();
        let entries = [
            Entry {
                object: event(),
                rights: rights::READ,
            },
            Entry {
                object: event(),
                rights: rights::WRITE,
            },
        ];
        let mut seen = [HANDLE_INVALID; 2];
        assert_eq!(
            t.insert_many_then::<4>(&entries, Some(&root), |handles| {
                seen.copy_from_slice(handles);
                Ok(())
            }),
            Ok(())
        );
        assert_eq!((t.len(), t.slots.len(), t.free.len()), (3, 3, 0));
        assert_eq!(kernel_used(&root), 3 * SLOT_COST);
        assert_eq!(t.get(live).unwrap().rights, rights::WAIT);
        assert_eq!(t.get(old).err(), Some(Error::BadHandle));
        assert_ne!(seen[0], old);
        assert_eq!(seen[0] >> GENERATION_BITS, old >> GENERATION_BITS);
        for (handle, entry) in seen.into_iter().zip(&entries) {
            let installed = t.get(handle).unwrap();
            assert_eq!(installed.rights, entry.rights);
            assert!(Arc::ptr_eq(&installed.object, &entry.object));
            assert_eq!(Arc::strong_count(&entry.object), 2);
        }
        drop(t);
        assert_eq!(kernel_used(&root), 0);
    }

    #[test]
    fn empty_batch_publishes_once() {
        let mut t = HandleTable::new();
        let mut calls = 0;
        assert_eq!(
            t.insert_many_then::<0>(&[], None, |handles| {
                calls += 1;
                assert!(handles.is_empty());
                Ok(())
            }),
            Ok(())
        );
        assert_eq!(calls, 1);
        assert!(t.is_empty());
        assert!(t.slots.is_empty());
        assert!(t.free.is_empty());
    }

    #[test]
    fn never_reissues_a_closed_value_at_once() {
        let mut t = HandleTable::new();
        let a = t.insert(event(), 0, None).unwrap();
        assert_ne!(a, HANDLE_INVALID);
        t.close(a).unwrap();
        assert_eq!(t.get(a).err(), Some(Error::BadHandle));
        let b = t.insert(event(), 0, None).unwrap();
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
        let first = t.insert(event(), 0, None).unwrap();
        let mut h = first;
        for _ in 0..256 {
            t.close(h).unwrap();
            h = t.insert(event(), 0, None).unwrap();
        }
        assert_eq!(h, first);
    }

    #[test]
    fn checks_validity_type_and_rights_in_order() {
        let mut t = HandleTable::new();
        let h = t.insert(event(), rights::WAIT, None).unwrap();
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
        let h = t.insert(event(), all, None).unwrap();
        let koid = t.get(h).unwrap().object.header().koid();

        let d = t.duplicate(h, rights::SAME_RIGHTS, None).unwrap();
        assert_eq!(t.get(d).unwrap().rights, all);
        assert_eq!(t.get(d).unwrap().object.header().koid(), koid);
        assert_eq!(t.duplicate(h, rights::MAP, None), Err(Error::InvalidArgs));

        let r = t.replace(d, rights::WAIT).unwrap();
        assert_eq!(t.get(d).err(), Some(Error::BadHandle));
        assert_eq!(t.get(r).unwrap().rights, rights::WAIT);
        // Without DUPLICATE, duplicating is denied but replacing works.
        assert_eq!(t.duplicate(r, rights::WAIT, None), Err(Error::AccessDenied));
        assert_eq!(t.replace(r, rights::SIGNAL), Err(Error::InvalidArgs));
        assert!(t.get(r).is_ok());
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn enforces_the_limit_and_frees_objects() {
        let mut t = HandleTable::new();
        let object = event();
        let handles: Vec<Handle> = (0..HANDLE_LIMIT)
            .map(|_| t.insert(object.clone(), rights::DUPLICATE, None).unwrap())
            .collect();
        assert_eq!(t.insert(object.clone(), 0, None), Err(Error::NoResources));
        assert_eq!(
            t.duplicate(handles[0], rights::SAME_RIGHTS, None),
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

    #[test]
    fn charges_new_slots_to_the_given_budget() {
        let root = Budget::root();
        let small = Budget::child(&root, SLOT_COST, UNLIMITED, None).unwrap();
        let other = Budget::child(&root, UNLIMITED, UNLIMITED, None).unwrap();
        let used = |b: &Budget| u64::from_le_bytes(b.info()[8..16].try_into().unwrap());
        let mut t = HandleTable::new();
        let a = t.insert(event(), rights::DUPLICATE, Some(&small)).unwrap();
        assert_eq!(used(&small), SLOT_COST);
        assert_eq!(
            t.duplicate(a, rights::SAME_RIGHTS, Some(&small)),
            Err(Error::NoResources)
        );
        assert_eq!(t.len(), 1);
        let fail = |_| Err(Error::InvalidArgs);
        assert_eq!(
            t.duplicate_then(a, rights::SAME_RIGHTS, Some(&other), fail),
            Err(Error::InvalidArgs)
        );
        assert_eq!(
            t.insert_then(event(), 0, Some(&other), fail),
            Err(Error::InvalidArgs)
        );
        assert_eq!((used(&other), t.len()), (0, 1));
        let b = t.duplicate(a, rights::SAME_RIGHTS, Some(&other)).unwrap();
        assert_eq!(used(&other), SLOT_COST);
        // A freed slot is reused without a new charge.
        t.close(b).unwrap();
        t.insert(event(), 0, Some(&small)).unwrap();
        t.replace(a, 0).unwrap();
        assert_eq!((used(&small), used(&other)), (SLOT_COST, SLOT_COST));
        drop(t);
        assert_eq!((used(&small), used(&other), used(&root)), (0, 0, 0));
    }
}
