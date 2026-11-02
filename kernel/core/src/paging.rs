//! The state of a paged memory object's pages that the pager drives:
//! requests, failures, revocation, dirty tracking and writeback.

use crate::status::Error;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

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
    recorded: BTreeMap<u64, u64>,
    written: BTreeSet<u64>,
}

/// The pager state of one paged memory object, by page index. The frames
/// themselves are kept elsewhere; this records the generation of each
/// present page, a number no other page of the object ever had.
#[derive(Debug, Default)]
pub struct Paging {
    track_dirty: bool,
    generations: BTreeMap<u64, u64>,
    next_generation: u64,
    requested: BTreeSet<u64>,
    failed: BTreeSet<u64>,
    dirty: BTreeSet<u64>,
    writeback: Option<Writeback>,
    revoked: bool,
    pager_gone: bool,
}

impl Paging {
    pub fn new(track_dirty: bool) -> Paging {
        Paging {
            track_dirty,
            ..Paging::default()
        }
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
    /// counts as queued from then on.
    pub fn absent(&mut self, index: u64) -> Absent {
        if self.revoked {
            Absent::Revoked
        } else if self.failed.contains(&index) || self.pager_gone {
            Absent::Failed
        } else if self.requested.insert(index) {
            Absent::Request
        } else {
            Absent::Wait
        }
    }

    /// Records the supply of the absent page `index`: it is present and
    /// clean, and its request is answered. Fails with `AccessDenied` once
    /// the object is revoked; returns false when the page was present.
    pub fn supply(&mut self, index: u64) -> Result<bool, Error> {
        if self.revoked {
            return Err(Error::AccessDenied);
        }
        if self.present(index) {
            return Ok(false);
        }
        self.requested.remove(&index);
        self.failed.remove(&index);
        self.dirty.remove(&index);
        self.generations.insert(index, self.next_generation);
        self.next_generation += 1;
        Ok(true)
    }

    /// Makes the absent pages from `first` to `end` fail, as `FAIL` does.
    pub fn fail(&mut self, first: u64, end: u64) {
        for index in first..end {
            if !self.present(index) {
                self.requested.remove(&index);
                self.failed.insert(index);
            }
        }
    }

    /// Makes every unanswered and later request fail, as when the pager
    /// is destroyed.
    pub fn pager_gone(&mut self) {
        self.pager_gone = true;
        self.requested.clear();
    }

    /// Revokes the object for good.
    pub fn revoke(&mut self) {
        self.revoked = true;
        self.requested.clear();
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
    }

    /// Records a write to the present page `index`.
    pub fn written(&mut self, index: u64) {
        if !self.track_dirty {
            return;
        }
        self.dirty.insert(index);
        if let Some(w) = &mut self.writeback
            && w.recorded.contains_key(&index)
        {
            w.written.insert(index);
        }
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

    /// The dirty ranges from `first` to `end`, as page index ranges.
    pub fn dirty_ranges(&self, first: u64, end: u64) -> Vec<(u64, u64)> {
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for &index in self.dirty.range(first..end) {
            match ranges.last_mut() {
                Some(r) if r.1 == index => r.1 += 1,
                _ => ranges.push((index, index + 1)),
            }
        }
        ranges
    }

    /// Begins a writeback of the pages from `first` to `end`, recording
    /// the dirty ones, as `WRITEBACK_BEGIN` does. Returns the recorded
    /// pages, whose mappings must stop being writable without a fault.
    pub fn writeback_begin(&mut self, first: u64, end: u64) -> Result<Vec<u64>, Error> {
        self.need_dirty_tracking()?;
        if self.writeback.is_some() {
            return Err(Error::BadState);
        }
        let recorded: BTreeMap<u64, u64> = self
            .dirty
            .range(first..end)
            .filter_map(|&i| self.generations.get(&i).map(|&g| (i, g)))
            .collect();
        let pages = recorded.keys().copied().collect();
        self.writeback = Some(Writeback {
            first,
            end,
            recorded,
            written: BTreeSet::new(),
        });
        Ok(pages)
    }

    /// Ends the writeback of exactly `first` to `end`: with `persisted`,
    /// recorded pages that are still the same page and were not written
    /// since become clean, as `WRITEBACK_END` does; without it they stay
    /// dirty, as `WRITEBACK_ABORT` does.
    pub fn writeback_end(&mut self, first: u64, end: u64, persisted: bool) -> Result<(), Error> {
        self.need_dirty_tracking()?;
        match &self.writeback {
            Some(w) if w.first == first && w.end == end => {}
            _ => return Err(Error::BadState),
        }
        let w = self.writeback.take().expect("checked above");
        if persisted {
            for (index, generation) in w.recorded {
                let same = self.generations.get(&index) == Some(&generation);
                if same && !w.written.contains(&index) {
                    self.dirty.remove(&index);
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    #[test]
    fn requests_each_absent_page_once() {
        let mut p = Paging::new(false);
        assert_eq!(p.absent(3), Absent::Request);
        assert_eq!(p.absent(3), Absent::Wait);
        assert_eq!(p.supply(3), Ok(true));
        assert_eq!(p.supply(3), Ok(false));
        assert!(p.present(3));
        p.removed(3);
        assert_eq!(p.absent(3), Absent::Request);
    }

    #[test]
    fn fails_absent_pages() {
        let mut p = Paging::new(false);
        p.supply(1).unwrap();
        assert_eq!(p.absent(2), Absent::Request);
        p.fail(0, 4);
        assert!(p.present(1));
        assert_eq!(p.absent(2), Absent::Failed);
        // Supplying a failed page makes it present again.
        p.supply(2).unwrap();
        assert!(p.present(2));
        p.pager_gone();
        assert_eq!(p.absent(5), Absent::Failed);
    }

    #[test]
    fn revokes_for_good() {
        let mut p = Paging::new(true);
        p.supply(0).unwrap();
        p.revoke();
        assert_eq!(p.absent(1), Absent::Revoked);
        assert_eq!(p.supply(1), Err(Error::AccessDenied));
    }

    #[test]
    fn tracks_dirty_pages_through_writeback() {
        let mut p = Paging::new(true);
        for i in 0..4 {
            p.supply(i).unwrap();
        }
        assert!(!p.writable_without_fault(0));
        p.written(0);
        p.written(1);
        p.written(3);
        assert_eq!(p.dirty_ranges(0, 4), vec![(0, 2), (3, 4)]);
        assert_eq!(p.writeback_begin(0, 2), Ok(vec![0, 1]));
        assert_eq!(p.writeback_begin(0, 4), Err(Error::BadState));
        assert!(!p.writable_without_fault(0) && !p.evictable(0));
        p.written(1);
        assert!(p.writable_without_fault(1));
        assert_eq!(p.writeback_end(0, 3, true), Err(Error::BadState));
        p.writeback_end(0, 2, true).unwrap();
        // Page 1 was written during the writeback and stays dirty.
        assert_eq!(p.dirty_ranges(0, 4), vec![(1, 2), (3, 4)]);
        assert!(p.evictable(0));
    }

    #[test]
    fn keeps_replaced_and_aborted_pages_dirty() {
        let mut p = Paging::new(true);
        p.supply(0).unwrap();
        p.supply(1).unwrap();
        p.written(0);
        p.written(1);
        p.writeback_begin(0, 2).unwrap();
        // Page 0 is removed and recreated, as by a shrink and a regrow.
        p.removed(0);
        p.supply(0).unwrap();
        p.written(0);
        p.writeback_end(0, 2, true).unwrap();
        assert_eq!(p.dirty_ranges(0, 2), vec![(0, 1)]);
        p.written(1);
        p.writeback_begin(0, 2).unwrap();
        p.writeback_end(0, 2, false).unwrap();
        assert_eq!(p.dirty_ranges(0, 2), vec![(0, 2)]);
    }

    #[test]
    fn needs_dirty_tracking_for_writeback() {
        let mut p = Paging::new(false);
        assert_eq!(p.writeback_begin(0, 1), Err(Error::NotSupported));
        assert_eq!(p.writeback_end(0, 1, true), Err(Error::NotSupported));
        p.supply(0).unwrap();
        p.written(0);
        assert!(p.writable_without_fault(0) && p.evictable(0));
    }
}
