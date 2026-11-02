//! The state of a paged memory object's pages that the pager drives:
//! requests, failures, revocation, dirty tracking and writeback.

use crate::nodes::{self, INSERT, Map, Set};
use crate::object::budget::Budget;
use crate::status::Error;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// The kernel memory charged for each entry of the state: the entry and
/// its share of the tree's nodes. A present page counts as two entries,
/// its generation and the record of the budget its frame is charged to.
pub const STATE_ENTRY_COST: u64 = 2 * size_of::<(u64, u64)>() as u64;

/// What an access to an absent page has to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Absent {
    /// Queue a request for the page, then wait.
    Request,
    /// Wait for the request already queued.
    Wait,
    /// The page failed or its pager is gone.
    Failed,
    /// The object is revoked.
    Revoked,
}

/// An outstanding writeback: its range, the pages it recorded with their
/// generations, and the recorded pages written since.
#[derive(Debug)]
struct Writeback {
    first: u64,
    end: u64,
    recorded: Map<u64, u64>,
    written: Set<u64>,
    /// The page from which recording goes on, while it does.
    recording: Option<u64>,
    /// Whether the end was persisted, once the writeback is ending.
    ending: Option<bool>,
}

/// The pager state of one paged memory object, by page index. The frames
/// themselves are kept elsewhere; this records the generation of each
/// present page, a number no other page of the object ever had.
#[derive(Debug)]
pub struct Paging {
    track_dirty: bool,
    generations: Map<u64, u64>,
    next_generation: u64,
    requested: Set<u64>,
    failed: Set<u64>,
    dirty: Set<u64>,
    writeback: Option<Writeback>,
    revoked: bool,
    pager_gone: bool,
    /// The budget the entries are charged to, if any, and how many are
    /// charged, which is never fewer than there are.
    budget: Option<Arc<Budget>>,
    charged: u64,
}

impl Paging {
    pub fn new(track_dirty: bool) -> Paging {
        Paging {
            budget: None,
            charged: 0,
            track_dirty,
            generations: nodes::map(),
            next_generation: 0,
            requested: nodes::set(),
            failed: nodes::set(),
            dirty: nodes::set(),
            writeback: None,
            revoked: false,
            pager_gone: false,
        }
    }

    /// Charges the entries of the state to `budget` from now on.
    pub fn charged_to(mut self, budget: Arc<Budget>) -> Paging {
        debug_assert_eq!(self.charged, 0);
        self.budget = Some(budget);
        self
    }

    /// The number of entries the state holds.
    fn entries(&self) -> u64 {
        let writeback = self
            .writeback
            .as_ref()
            .map_or(0, |w| w.recorded.len() + w.written.len());
        (2 * self.generations.len()
            + self.requested.len()
            + self.failed.len()
            + self.dirty.len()
            + writeback) as u64
    }

    /// Charges room for `count` more entries, or fails with no effect.
    fn reserve(&mut self, count: u64) -> Result<(), Error> {
        if let Some(budget) = &self.budget {
            budget.charge_kernel(count * STATE_ENTRY_COST)?;
        }
        self.charged += count;
        Ok(())
    }

    /// Releases the charges of the entries reserved but not there.
    fn settle(&mut self) {
        let extra = self.charged - self.entries();
        if let Some(budget) = &self.budget
            && extra > 0
        {
            budget.release_kernel(extra * STATE_ENTRY_COST);
        }
        self.charged -= extra;
    }

    pub fn track_dirty(&self) -> bool {
        self.track_dirty
    }

    pub fn revoked(&self) -> bool {
        self.revoked
    }

    /// Whether page `index` is present.
    pub fn present(&self, index: u64) -> bool {
        self.generations.contains_key(&index)
    }

    /// What an access to the absent page `index` has to do. A `Request`
    /// counts as queued from then on. Fails with `NoMemory` without room
    /// to record the request.
    pub fn absent(&mut self, index: u64) -> Result<Absent, Error> {
        Ok(if self.revoked {
            Absent::Revoked
        } else if self.failed.contains(&index) || self.pager_gone {
            Absent::Failed
        } else if self.requested.contains(&index) {
            Absent::Wait
        } else {
            let _claim = nodes::claim_insert()?;
            self.reserve(1)?;
            self.requested.insert(index);
            Absent::Request
        })
    }

    /// Records the supply of the absent page `index`: it is present and
    /// clean, and its request is answered. Fails with `AccessDenied` once
    /// the object is revoked, and with `NoMemory`, changing nothing,
    /// without room to record it; returns false when the page was present.
    pub fn supply(&mut self, index: u64) -> Result<bool, Error> {
        if self.revoked {
            return Err(Error::AccessDenied);
        }
        if self.present(index) {
            return Ok(false);
        }
        let _claim = nodes::claim_insert()?;
        self.reserve(2)?;
        self.requested.remove(&index);
        self.failed.remove(&index);
        self.dirty.remove(&index);
        self.generations.insert(index, self.next_generation);
        self.next_generation += 1;
        self.settle();
        Ok(true)
    }

    /// Makes the absent pages from `first` to `end` fail, as `FAIL` does.
    /// Fails with `NoMemory` at the first page there is no room to record
    /// as failed, whose request is then forgotten, so that the next access
    /// requests it again.
    pub fn fail(&mut self, first: u64, end: u64) -> Result<(), Error> {
        let mut result = Ok(());
        for index in first..end {
            if !self.present(index) {
                self.requested.remove(&index);
                let claim = nodes::claim_insert().and_then(|c| self.reserve(1).map(|()| c));
                if let Err(e) = claim {
                    result = Err(e);
                    break;
                }
                self.failed.insert(index);
            }
        }
        self.settle();
        result
    }

    /// Forgets the request for page `index`, so that the next access
    /// requests it again.
    pub fn withdraw(&mut self, index: u64) {
        self.requested.remove(&index);
        self.settle();
    }

    /// Makes every unanswered and later request fail, as when the pager
    /// is destroyed.
    pub fn pager_gone(&mut self) {
        self.pager_gone = true;
        self.requested.clear();
        self.settle();
    }

    /// Revokes the object for good.
    pub fn revoke(&mut self) {
        self.revoked = true;
        self.requested.clear();
        self.settle();
    }

    /// Whether page `index` may be evicted: present, clean and not part
    /// of the outstanding writeback.
    pub fn evictable(&self, index: u64) -> bool {
        let in_writeback = self
            .writeback
            .as_ref()
            .is_some_and(|w| w.recorded.contains_key(&index));
        self.present(index) && !self.dirty.contains(&index) && !in_writeback
    }

    /// Records that page `index` left the object: evicted, decommitted or
    /// cut off by a shrink. It keeps its dirty state only while it is
    /// recorded by the writeback, whose data may not have reached the
    /// pager yet.
    pub fn removed(&mut self, index: u64) {
        self.generations.remove(&index);
        if !self
            .writeback
            .as_ref()
            .is_some_and(|w| w.recorded.contains_key(&index))
        {
            self.dirty.remove(&index);
        }
        self.settle();
    }

    /// Records a write to the present page `index`. Fails with
    /// `NoMemory`, changing nothing, without room to record it.
    pub fn written(&mut self, index: u64) -> Result<(), Error> {
        if !self.track_dirty {
            return Ok(());
        }
        let _claim = nodes::claim(2 * INSERT)?;
        self.reserve(2)?;
        self.dirty.insert(index);
        if let Some(w) = &mut self.writeback
            && w.recorded.contains_key(&index)
        {
            w.written.insert(index);
        }
        self.settle();
        Ok(())
    }

    /// Whether a mapping may let page `index` be written without a fault
    /// first: always without dirty tracking, and otherwise only once the
    /// page is dirty and any writeback recording it has seen a write.
    pub fn writable_without_fault(&self, index: u64) -> bool {
        if !self.track_dirty {
            return true;
        }
        let pending = self
            .writeback
            .as_ref()
            .is_some_and(|w| w.recorded.contains_key(&index) && !w.written.contains(&index));
        self.dirty.contains(&index) && !pending
    }

    /// The dirty pages from `first` to `end`, in order.
    pub fn dirty(&self, first: u64, end: u64) -> impl Iterator<Item = u64> + '_ {
        self.dirty.range(first..end).copied()
    }

    /// The dirty ranges from `first` to `end`, as page index ranges.
    /// Fails with `NoMemory` without room for them.
    pub fn dirty_ranges(&self, first: u64, end: u64) -> Result<Vec<(u64, u64)>, Error> {
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for &index in self.dirty.range(first..end) {
            match ranges.last_mut() {
                Some(r) if r.1 == index => r.1 += 1,
                _ => {
                    ranges.try_reserve(1).map_err(|_| Error::NoMemory)?;
                    ranges.push((index, index + 1));
                }
            }
        }
        Ok(ranges)
    }

    /// Opens a writeback of the pages from `first` to `end`, as
    /// `WRITEBACK_BEGIN` does, which records their dirty pages in steps
    /// of `writeback_record`. It cannot end before they are recorded.
    pub fn writeback_open(&mut self, first: u64, end: u64) -> Result<(), Error> {
        self.need_dirty_tracking()?;
        if self.writeback.is_some() {
            return Err(Error::BadState);
        }
        self.writeback = Some(Writeback {
            first,
            end,
            recorded: nodes::map(),
            written: nodes::set(),
            recording: Some(first),
            ending: None,
        });
        Ok(())
    }

    /// Records up to `limit` more dirty pages of the opening writeback.
    /// Returns the pages recorded, whose mappings must stop being
    /// writable without a fault, and whether every page is recorded. The
    /// charges of their records cover the list.
    /// Fails with `NoMemory` without room to record them, which ends the
    /// writeback.
    pub fn writeback_record(&mut self, limit: usize) -> Result<(Vec<u64>, bool), Error> {
        let Some(w) = self.writeback.as_ref() else {
            return Err(Error::BadState);
        };
        let Some(at) = w.recording else {
            return Ok((Vec::new(), true));
        };
        let end = w.end;
        let mut pages = Vec::new();
        let mut seen = 0;
        let mut next = end;
        let mut failed = None;
        let mut cursor = at;
        while seen < limit {
            let Some(&index) = self.dirty.range(cursor..end).next() else {
                break;
            };
            seen += 1;
            next = index + 1;
            cursor = next;
            if let Some(&generation) = self.generations.get(&index) {
                let Ok(_claim) = nodes::claim_insert() else {
                    failed = Some(Error::NoMemory);
                    break;
                };
                if pages.try_reserve(1).is_err() {
                    failed = Some(Error::NoMemory);
                    break;
                }
                if let Err(e) = self.reserve(1) {
                    failed = Some(e);
                    break;
                }
                let w = self.writeback.as_mut().expect("opened");
                w.recorded.insert(index, generation);
                pages.push(index);
            }
        }
        if let Some(e) = failed {
            self.writeback = None;
            self.settle();
            return Err(e);
        }
        let done = seen < limit || next >= end;
        let w = self.writeback.as_mut().expect("opened");
        w.recording = (!done).then_some(next);
        Ok((pages, done))
    }

    /// Begins ending the writeback of exactly `first` to `end`, whose
    /// pages are all recorded: with `persisted`, recorded pages that are
    /// still the same page and were not written since become clean, as
    /// `WRITEBACK_END` does; without it they stay dirty, as
    /// `WRITEBACK_ABORT` does. The pages are handled in steps of
    /// `writeback_finish`.
    pub fn writeback_close(&mut self, first: u64, end: u64, persisted: bool) -> Result<(), Error> {
        self.need_dirty_tracking()?;
        match &mut self.writeback {
            Some(w)
                if w.first == first
                    && w.end == end
                    && w.recording.is_none()
                    && w.ending.is_none() =>
            {
                w.ending = Some(persisted);
                Ok(())
            }
            _ => Err(Error::BadState),
        }
    }

    /// Handles up to `limit` recorded pages of the ending writeback, and
    /// returns whether it has ended.
    pub fn writeback_finish(&mut self, limit: usize) -> bool {
        let Some(w) = self.writeback.as_mut() else {
            return true;
        };
        let persisted = w.ending == Some(true);
        for _ in 0..limit {
            let Some((index, generation)) = w.recorded.pop_first() else {
                break;
            };
            let written = w.written.remove(&index);
            let same = self.generations.get(&index) == Some(&generation);
            if persisted && same && !written {
                self.dirty.remove(&index);
            }
        }
        let ended = w.recorded.is_empty();
        if ended {
            self.writeback = None;
        }
        self.settle();
        ended
    }

    /// Begins a writeback of the pages from `first` to `end` and records
    /// all of their dirty pages at once. Returns the recorded pages.
    pub fn writeback_begin(&mut self, first: u64, end: u64) -> Result<Vec<u64>, Error> {
        self.writeback_open(first, end)?;
        let mut all = Vec::new();
        loop {
            let (pages, done) = self.writeback_record(usize::MAX)?;
            all.extend(pages);
            if done {
                return Ok(all);
            }
        }
    }

    /// Ends the writeback of exactly `first` to `end` at once, as
    /// `writeback_close` and `writeback_finish` do.
    pub fn writeback_end(&mut self, first: u64, end: u64, persisted: bool) -> Result<(), Error> {
        self.writeback_close(first, end, persisted)?;
        while !self.writeback_finish(usize::MAX) {}
        Ok(())
    }

    pub fn need_dirty_tracking(&self) -> Result<(), Error> {
        if self.track_dirty {
            Ok(())
        } else {
            Err(Error::NotSupported)
        }
    }
}

impl Drop for Paging {
    fn drop(&mut self) {
        if let Some(budget) = &self.budget {
            budget.release_kernel(self.charged * STATE_ENTRY_COST);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    #[test]
    fn requests_each_absent_page_once() {
        let mut p = Paging::new(false);
        assert_eq!(p.absent(3), Ok(Absent::Request));
        assert_eq!(p.absent(3), Ok(Absent::Wait));
        assert_eq!(p.supply(3), Ok(true));
        assert_eq!(p.supply(3), Ok(false));
        assert!(p.present(3));
        p.removed(3);
        assert_eq!(p.absent(3), Ok(Absent::Request));
    }

    #[test]
    fn charges_entries_until_they_go() {
        use crate::object::budget::UNLIMITED;
        let root = Budget::root();
        let small = Budget::child(&root, 3 * STATE_ENTRY_COST, UNLIMITED, None).unwrap();
        let used = |b: &Budget| u64::from_le_bytes(b.info()[8..16].try_into().unwrap());
        let mut p = Paging::new(true).charged_to(small.clone());
        assert_eq!(p.absent(0), Ok(Absent::Request));
        assert_eq!(used(&small), STATE_ENTRY_COST);
        // The request makes way for the present page.
        assert_eq!(p.supply(0), Ok(true));
        assert_eq!(used(&small), 2 * STATE_ENTRY_COST);
        // Over the limit, nothing changes.
        assert_eq!(p.written(0), Err(Error::NoResources));
        assert!(!p.writable_without_fault(0));
        assert_eq!(p.fail(1, 3), Err(Error::NoResources));
        assert_eq!(p.absent(1), Ok(Absent::Failed));
        assert_eq!(p.absent(2), Err(Error::NoResources));
        assert_eq!(used(&small), 3 * STATE_ENTRY_COST);
        p.removed(0);
        assert_eq!(p.absent(2), Ok(Absent::Request));
        assert_eq!(used(&small), 2 * STATE_ENTRY_COST);
        p.withdraw(2);
        assert_eq!(used(&small), STATE_ENTRY_COST);
        drop(p);
        assert_eq!((used(&small), used(&root)), (0, 0));
    }

    #[test]
    fn fails_absent_pages() {
        let mut p = Paging::new(false);
        p.supply(1).unwrap();
        assert_eq!(p.absent(2), Ok(Absent::Request));
        p.fail(0, 4).unwrap();
        assert!(p.present(1));
        assert_eq!(p.absent(2), Ok(Absent::Failed));
        // Supplying a failed page makes it present again.
        p.supply(2).unwrap();
        assert!(p.present(2));
        p.pager_gone();
        assert_eq!(p.absent(5), Ok(Absent::Failed));
    }

    #[test]
    fn revokes_for_good() {
        let mut p = Paging::new(true);
        p.supply(0).unwrap();
        p.revoke();
        assert_eq!(p.absent(1), Ok(Absent::Revoked));
        assert_eq!(p.supply(1), Err(Error::AccessDenied));
    }

    #[test]
    fn tracks_dirty_pages_through_writeback() {
        let mut p = Paging::new(true);
        for i in 0..4 {
            p.supply(i).unwrap();
        }
        assert!(!p.writable_without_fault(0));
        p.written(0).unwrap();
        p.written(1).unwrap();
        p.written(3).unwrap();
        assert_eq!(p.dirty_ranges(0, 4), Ok(vec![(0, 2), (3, 4)]));
        assert_eq!(p.writeback_begin(0, 2), Ok(vec![0, 1]));
        assert_eq!(p.writeback_begin(0, 4), Err(Error::BadState));
        assert!(!p.writable_without_fault(0) && !p.evictable(0));
        p.written(1).unwrap();
        assert!(p.writable_without_fault(1));
        assert_eq!(p.writeback_end(0, 3, true), Err(Error::BadState));
        p.writeback_end(0, 2, true).unwrap();
        // Page 1 was written during the writeback and stays dirty.
        assert_eq!(p.dirty_ranges(0, 4), Ok(vec![(1, 2), (3, 4)]));
        assert!(p.evictable(0));
    }

    #[test]
    fn keeps_replaced_and_aborted_pages_dirty() {
        let mut p = Paging::new(true);
        p.supply(0).unwrap();
        p.supply(1).unwrap();
        p.written(0).unwrap();
        p.written(1).unwrap();
        p.writeback_begin(0, 2).unwrap();
        // Page 0 is removed and recreated, as by a shrink and a regrow.
        p.removed(0);
        p.supply(0).unwrap();
        p.written(0).unwrap();
        p.writeback_end(0, 2, true).unwrap();
        assert_eq!(p.dirty_ranges(0, 2), Ok(vec![(0, 1)]));
        p.written(1).unwrap();
        p.writeback_begin(0, 2).unwrap();
        p.writeback_end(0, 2, false).unwrap();
        assert_eq!(p.dirty_ranges(0, 2), Ok(vec![(0, 2)]));
    }

    #[test]
    fn records_and_ends_writebacks_in_steps() {
        let mut p = Paging::new(true);
        for i in 0..4 {
            p.supply(i).unwrap();
            p.written(i).unwrap();
        }
        p.writeback_open(0, 4).unwrap();
        assert_eq!(p.writeback_record(3), Ok((vec![0, 1, 2], false)));
        // Recording is not done, so the writeback cannot end yet.
        assert_eq!(p.writeback_close(0, 4, true), Err(Error::BadState));
        assert_eq!(p.writeback_record(3), Ok((vec![3], true)));
        p.writeback_close(0, 4, true).unwrap();
        assert_eq!(p.writeback_close(0, 4, true), Err(Error::BadState));
        assert!(!p.writeback_finish(2));
        // Page 3, written while the end is under way, stays dirty; the
        // others become clean.
        p.written(3).unwrap();
        assert!(p.writeback_finish(2));
        assert_eq!(p.dirty_ranges(0, 4), Ok(vec![(3, 4)]));
        assert_eq!(p.writeback_begin(0, 4), Ok(vec![3]));
    }

    #[test]
    fn ends_a_writeback_that_cannot_record() {
        let mut p = Paging::new(true);
        p.supply(0).unwrap();
        p.written(0).unwrap();
        p.writeback_open(0, 1).unwrap();
        let recorded = crate::nodes::tests::without_claims(|| p.writeback_record(8));
        assert_eq!(recorded, Err(Error::NoMemory));
        assert_eq!(p.writeback_end(0, 1, true), Err(Error::BadState));
        assert_eq!(p.writeback_begin(0, 1), Ok(vec![0]));
    }

    #[test]
    fn needs_dirty_tracking_for_writeback() {
        let mut p = Paging::new(false);
        assert_eq!(p.writeback_begin(0, 1), Err(Error::NotSupported));
        assert_eq!(p.writeback_end(0, 1, true), Err(Error::NotSupported));
        p.supply(0).unwrap();
        p.written(0).unwrap();
        assert!(p.writable_without_fault(0) && p.evictable(0));
    }
}
