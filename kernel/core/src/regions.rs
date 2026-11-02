//! The regions of a user address space: which range maps which part of
//! which memory object, and with what permissions.

use crate::gaps::{GAP_SIZE, Gaps};
use crate::nodes::{self, INSERT, Map};
use crate::object::budget::Budget;
use crate::object::rights;
use crate::status::Error;
use alloc::sync::Arc;
use alloc::vec::Vec;

pub const PAGE_SIZE: u64 = 4096;

pub const PERM_READ: u32 = 1 << 0;
pub const PERM_WRITE: u32 = 1 << 1;
pub const PERM_EXECUTE: u32 = 1 << 2;
pub const SPECIFIC: u32 = 1 << 8;
const INHERITANCE_SHIFT: u32 = 9;
const INHERITANCE_MASK: u32 = 3 << INHERITANCE_SHIFT;
pub const BELOW_4G: u32 = 1 << 11;
const PERMS: u32 = PERM_READ | PERM_WRITE | PERM_EXECUTE;
const DEFINED: u32 = PERMS | SPECIFIC | INHERITANCE_MASK | BELOW_4G;

/// The checked `options` of `vm_map`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapOptions {
    pub perms: u32,
    pub specific: bool,
    pub inheritance: u32,
    pub below_4g: bool,
}

impl MapOptions {
    /// Checks `options` together with the `addr` they come with. Mappings
    /// that are writable and executable are refused unless
    /// `write_execute` allows them.
    pub fn parse(options: u32, addr: u64, write_execute: bool) -> Result<MapOptions, Error> {
        let perms = options & PERMS;
        let inheritance = (options & INHERITANCE_MASK) >> INHERITANCE_SHIFT;
        let specific = options & SPECIFIC != 0;
        let below_4g = options & BELOW_4G != 0;
        let unreadable = perms & (PERM_WRITE | PERM_EXECUTE) != 0 && perms & PERM_READ == 0;
        let refused = !write_execute && perms & PERM_WRITE != 0 && perms & PERM_EXECUTE != 0;
        let bad_addr = if specific {
            !addr.is_multiple_of(PAGE_SIZE)
        } else {
            addr != 0
        };
        if options & !DEFINED != 0
            || unreadable
            || refused
            || inheritance == 3
            || (specific && below_4g)
            || bad_addr
        {
            return Err(Error::InvalidArgs);
        }
        Ok(MapOptions {
            perms,
            specific,
            inheritance,
            below_4g,
        })
    }

    /// The permissions of `vm_protect` `options`: only permission bits,
    /// readable whenever writable or executable, and not both writable
    /// and executable unless `write_execute` allows it.
    pub fn parse_protection(options: u32, write_execute: bool) -> Result<u32, Error> {
        let unreadable = options & (PERM_WRITE | PERM_EXECUTE) != 0 && options & PERM_READ == 0;
        let refused = !write_execute && options & PERM_WRITE != 0 && options & PERM_EXECUTE != 0;
        if options & !PERMS != 0 || unreadable || refused {
            return Err(Error::InvalidArgs);
        }
        Ok(options)
    }

    /// The permissions that a handle with `rights` allows.
    pub fn allowed_by(rights: u32) -> u32 {
        [
            (PERM_READ, rights::READ),
            (PERM_WRITE, rights::WRITE),
            (PERM_EXECUTE, rights::EXECUTE),
        ]
        .iter()
        .filter(|(_, right)| rights & right != 0)
        .fold(0, |perms, (perm, _)| perms | perm)
    }

    /// The rights the memory object's handle needs for the permissions.
    pub fn rights(&self) -> u32 {
        let mut needed = 0;
        for (perm, right) in [
            (PERM_READ, rights::READ),
            (PERM_WRITE, rights::WRITE),
            (PERM_EXECUTE, rights::EXECUTE),
        ] {
            if self.perms & perm != 0 {
                needed |= right;
            }
        }
        needed
    }
}

/// The bytes a region's record and its entry in the memory object may take.
pub const RECORD_ROOM: usize = 64;

/// The kernel memory charged for a region: its entry and that entry's
/// share of the map storage, its record in its memory object, and room
/// for a gap in each gap tree, each with its share of the storage that
/// holds it.
pub const REGION_COST: u64 = 2
    * (size_of::<u64>() + size_of::<Region<Arc<()>, usize>>() + RECORD_ROOM + 2 * GAP_SIZE) as u64;

/// A region's entry charged to a budget, which is released when the
/// region goes.
#[derive(Default)]
pub struct Charge(Option<Arc<Budget>>);

impl Charge {
    /// No charge.
    pub const NONE: Charge = Charge(None);

    /// Charges one region to `budget`, if there is one.
    pub fn new(budget: Option<&Arc<Budget>>) -> Result<Charge, Error> {
        match budget {
            Some(budget) => {
                budget.charge_kernel(REGION_COST)?;
                Ok(Charge(Some(budget.clone())))
            }
            None => Ok(Charge::NONE),
        }
    }
}

// Not a `Held`, which keeps the size too, so that a region's node of the
// map still fits in a reserve block with the region's record.
impl Drop for Charge {
    fn drop(&mut self) {
        if let Some(budget) = &self.0 {
            budget.release_kernel(REGION_COST);
        }
    }
}

impl core::fmt::Debug for Charge {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Charge"
        } else {
            "Charge::NONE"
        })
    }
}

/// What a region keeps in its memory object so that the object can find
/// its mappings. It goes when the region does.
pub trait Record: Sized {
    /// The record of the part from `start` of the region, which a split
    /// makes a region of its own, or an error, with no effect.
    fn split(&self, start: u64) -> Result<Self, Error>;

    /// Notes whether the region now maps the object writable.
    fn set_writable(&mut self, writable: bool);
}

/// No record, for regions whose objects need none.
impl Record for () {
    fn split(&self, _: u64) -> Result<(), Error> {
        Ok(())
    }

    fn set_writable(&mut self, _: bool) {}
}

/// A mapped range. `M` is the memory object and `R` its record there.
#[derive(Debug)]
pub struct Region<M, R = ()> {
    pub start: u64,
    pub len: u64,
    /// The offset in the object of the first byte.
    pub offset: u64,
    /// The permission bits of `vm_map`.
    pub perms: u32,
    /// The permissions the rights of the handle that made the mapping
    /// allow, which `vm_protect` may not go beyond.
    pub max: u32,
    pub inheritance: u32,
    pub mem: M,
    /// The removal under way that the region belongs to, or 0 while it
    /// stays. A region being removed keeps its range and object, but no
    /// page of it can be entered or changed.
    pub removal: u64,
    pub charge: Charge,
    pub record: R,
}

impl<M, R: Record> Region<M, R> {
    /// Sets the permissions, telling the record.
    pub fn set_perms(&mut self, perms: u32) {
        self.perms = perms;
        self.record.set_writable(perms & PERM_WRITE != 0);
    }
}

impl<M, R> Region<M, R> {
    pub fn end(&self) -> u64 {
        self.start + self.len
    }

    /// The page index in the object of the page at `addr`, which lies in
    /// the region.
    pub fn page(&self, addr: u64) -> u64 {
        (self.offset + (addr - self.start)) / PAGE_SIZE
    }
}

/// Non-overlapping regions, keyed by their start, inside a range, with
/// the free gaps between them in pages.
pub struct Regions<M, R = ()> {
    map: Map<u64, Region<M, R>>,
    /// The gaps below and above `line`; a gap across it is split there.
    /// Each tree has room for two more gaps than there are regions,
    /// which is as many as there can be, so that removing regions never
    /// allocates.
    below: Gaps,
    above: Gaps,
    /// The range the regions lie in and the line that `choose` can keep
    /// a range below, all page-aligned.
    low: u64,
    line: u64,
    high: u64,
    /// The ranges of the removals under way that refuse inserts.
    removing: Vec<(u64, u64)>,
}

/// A removal of the parts of the regions in a range, begun by
/// `Regions::begin_removal` and done in steps.
#[derive(Debug)]
pub struct Removal {
    id: u64,
    start: u64,
    end: u64,
    /// Where the next `mark_some` goes on.
    cursor: u64,
    /// Whether the removal refuses inserts into its range.
    blocks: bool,
}

impl<M: Clone, R: Record> Regions<M, R> {
    /// No regions in the range from `low` to `high`, with `line` between
    /// them, all page-aligned. Fails with `NoMemory` without room for the
    /// gaps.
    pub fn new(low: u64, line: u64, high: u64) -> Result<Regions<M, R>, Error> {
        debug_assert!(low <= line && line <= high);
        debug_assert!(
            [low, line, high]
                .iter()
                .all(|v| v.is_multiple_of(PAGE_SIZE))
        );
        let mut regions = Regions {
            map: nodes::map(),
            below: Gaps::new(),
            above: Gaps::new(),
            low,
            line,
            high,
            removing: Vec::new(),
        };
        regions.reserve_gaps(0)?;
        regions.relink(low, high, true);
        Ok(regions)
    }

    /// Makes room in each gap tree for as many gaps as there can be once
    /// `more` regions are added.
    fn reserve_gaps(&mut self, more: usize) -> Result<(), Error> {
        let most = self.map.len() + more + 2;
        for gaps in [&mut self.below, &mut self.above] {
            gaps.reserve(most.saturating_sub(gaps.len() as usize))?;
        }
        Ok(())
    }

    /// Adds the gaps from `from` to `to` to the trees, or with `add` false
    /// removes them. No gap may cross `from` or `to`: each is the end of a
    /// region or of the range, or the start of a region or of the range.
    fn relink(&mut self, from: u64, to: u64, add: bool) {
        let Regions {
            map,
            below,
            above,
            line,
            ..
        } = self;
        let put = |gaps: &mut Gaps, start: u64, end: u64| {
            if start < end {
                let (at, len) = (start / PAGE_SIZE, (end - start) / PAGE_SIZE);
                if add {
                    gaps.insert(at, len);
                } else {
                    gaps.remove(at, len);
                }
            }
        };
        let mut gap = |start: u64, end: u64| {
            put(below, start, end.min(*line));
            put(above, start.max(*line), end);
        };
        let mut at = from;
        for r in map.range(from..to).map(|(_, r)| r) {
            gap(at, r.start);
            at = r.end();
        }
        gap(at, to);
    }

    /// The end of the last region that starts before `start`, or the
    /// start of the range, and the start of the first region that starts
    /// at or after `end`, or the end of the range: bounds no gap crosses.
    fn window(&self, start: u64, end: u64) -> (u64, u64) {
        let from = self
            .map
            .range(..start)
            .next_back()
            .map_or(self.low, |(_, r)| r.end());
        let to = self
            .map
            .range(end..)
            .next()
            .map_or(self.high, |(_, r)| r.start);
        (from, to)
    }

    /// Checks that the gap trees hold exactly the gaps between the
    /// regions, for tests and fuzzing.
    pub fn check_gaps(&self) {
        let mut expected = [Vec::new(), Vec::new()];
        let mut gap = |start: u64, end: u64| {
            for (i, (a, b)) in [(start, end.min(self.line)), (start.max(self.line), end)]
                .into_iter()
                .enumerate()
            {
                if a < b {
                    expected[i].push((a / PAGE_SIZE, (b - a) / PAGE_SIZE));
                }
            }
        };
        let mut at = self.low;
        for r in self.map.values() {
            gap(at, r.start);
            at = r.end();
        }
        gap(at, self.high);
        for (gaps, mut expected) in [&self.below, &self.above].into_iter().zip(expected) {
            let mut listed = gaps.check();
            listed.sort_unstable();
            expected.sort_unstable();
            assert_eq!(listed, expected);
        }
    }

    /// The region containing `addr`, also one being removed.
    fn find_any(&self, addr: u64) -> Option<&Region<M, R>> {
        let (_, r) = self.map.range(..=addr).next_back()?;
        (addr < r.end()).then_some(r)
    }

    /// The region containing `addr`, unless it is being removed.
    pub fn find(&self, addr: u64) -> Option<&Region<M, R>> {
        self.find_any(addr).filter(|r| r.removal == 0)
    }

    /// The region that starts at `start`, also one being removed.
    pub fn starting_at(&self, start: u64) -> Option<&Region<M, R>> {
        self.map.get(&start)
    }

    /// Whether any region, also one being removed, overlaps the `len`
    /// bytes from `start`.
    pub fn overlaps(&self, start: u64, len: u64) -> bool {
        let end = start + len;
        self.find_any(start).is_some() || self.map.range(start..end).next().is_some()
    }

    /// Adds `region`, or fails with `OutOfRange` when it does not lie in
    /// the range, with `ShouldWait` when it overlaps the range of a
    /// removal under way, with `AlreadyExists` when it overlaps a region
    /// and with `NoMemory` without room for it.
    pub fn insert(&mut self, region: Region<M, R>) -> Result<(), Error> {
        let Some(end) = region.start.checked_add(region.len) else {
            return Err(Error::OutOfRange);
        };
        if region.start < self.low || end > self.high {
            return Err(Error::OutOfRange);
        }
        if self
            .removing
            .iter()
            .any(|&(s, e)| s < end && region.start < e)
        {
            return Err(Error::ShouldWait);
        }
        if self.overlaps(region.start, region.len) {
            return Err(Error::AlreadyExists);
        }
        self.reserve_gaps(1)?;
        let _claim = nodes::claim_insert()?;
        let (from, to) = self.window(region.start, region.end());
        self.relink(from, to, false);
        self.map.insert(region.start, region);
        self.relink(from, to, true);
        Ok(())
    }

    /// Claims room for splitting the regions at both ends of a range.
    fn claim_splits(&mut self) -> Result<nodes::Claim, Error> {
        self.reserve_gaps(2)?;
        nodes::claim(2 * INSERT)
    }

    /// Charges `budget`, if there is one, for the regions that splitting
    /// at `start` and then at `end` adds, and makes their records, which
    /// `split_ends` takes.
    fn charge_splits(
        &self,
        start: u64,
        end: u64,
        budget: Option<&Arc<Budget>>,
    ) -> Result<[Option<(Charge, R)>; 2], Error> {
        let split = |at: u64| -> Result<Option<(Charge, R)>, Error> {
            match self.find(at) {
                Some(r) if r.start != at => {
                    let charge = Charge::new(budget)?;
                    Ok(Some((charge, r.record.split(at)?)))
                }
                _ => Ok(None),
            }
        };
        let first = split(start)?;
        let second = if end != start { split(end)? } else { None };
        Ok([first, second])
    }

    /// Splits the regions at `start` and `end` with the charges and
    /// records from `charge_splits`. The caller holds a claim for the
    /// splits.
    fn split_ends(&mut self, start: u64, end: u64, [first, second]: [Option<(Charge, R)>; 2]) {
        if let Some(split) = first {
            self.split_at(start, split);
        }
        if let Some(split) = second {
            self.split_at(end, split);
        }
    }

    /// Splits the region containing `addr`, which is not being removed and
    /// does not start there, so that one starts there, giving the new
    /// region `charge` and `record`. The caller holds a claim for the
    /// split.
    fn split_at(&mut self, addr: u64, (charge, record): (Charge, R)) {
        let r = self.find(addr).expect("a region to split");
        debug_assert!(r.start != addr);
        let head_len = addr - r.start;
        let tail = Region {
            start: addr,
            len: r.len - head_len,
            offset: r.offset + head_len,
            perms: r.perms,
            max: r.max,
            inheritance: r.inheritance,
            mem: r.mem.clone(),
            removal: r.removal,
            charge,
            record,
        };
        let start = r.start;
        self.map.get_mut(&start).expect("found above").len = head_len;
        self.map.insert(addr, tail);
    }

    /// Begins removal `id`, not 0, of every part of a region inside the
    /// range from `start` to `end`, splitting regions that extend beyond
    /// it; `mark_some`, `next_marked` and `take_marked` do the rest in
    /// steps, and `end_removal` ends it. Regions that another removal is
    /// taking are left to it. Until it ends, inserts into the range fail
    /// with `ShouldWait`. Regions the splits add are charged to `budget`,
    /// if there is one. Fails with `NoMemory`, changing nothing, without
    /// room for the splits or to record the removal, and with
    /// `NoResources` when the budget refuses the charge.
    pub fn begin_removal(
        &mut self,
        start: u64,
        end: u64,
        id: u64,
        budget: Option<&Arc<Budget>>,
    ) -> Result<Removal, Error> {
        debug_assert!(id != 0);
        self.removing.try_reserve(1).map_err(|_| Error::NoMemory)?;
        self.split_range(start, end, budget)?;
        self.removing.push((start, end));
        Ok(Removal {
            id,
            start,
            end,
            cursor: start,
            blocks: true,
        })
    }

    /// Begins removal `id` of every region, as `begin_removal` does for
    /// the whole range, which needs no split. Without room to record the
    /// removal, inserts meanwhile are not refused.
    pub fn begin_removal_of_all(&mut self, id: u64) -> Removal {
        debug_assert!(id != 0);
        let blocks = self.removing.try_reserve(1).is_ok();
        if blocks {
            self.removing.push((self.low, self.high));
        }
        Removal {
            id,
            start: self.low,
            end: self.high,
            cursor: self.low,
            blocks,
        }
    }

    /// Marks the next regions of `removal` as being removed, looking at
    /// up to `limit` regions. Returns the range from where it started to
    /// where it stopped, or `None` once the whole range is done.
    pub fn mark_some(&mut self, removal: &mut Removal, limit: usize) -> Option<(u64, u64)> {
        let from = removal.cursor;
        if from >= removal.end {
            return None;
        }
        let mut to = removal.end;
        for (seen, r) in self
            .map
            .range_mut(from..removal.end)
            .map(|(_, r)| r)
            .enumerate()
        {
            if seen == limit {
                to = r.start;
                break;
            }
            if r.removal == 0 {
                r.removal = removal.id;
            }
        }
        removal.cursor = to;
        Some((from, to))
    }

    /// The start and length of the first region from `at` to `end` that
    /// `removal` marked.
    pub fn next_marked(&self, removal: &Removal, at: u64, end: u64) -> Option<(u64, u64)> {
        let mut regions = self.map.range(at..end).map(|(_, r)| r);
        let r = regions.find(|r| r.removal == removal.id)?;
        Some((r.start, r.len))
    }

    /// Takes out the first region from `at` to `end` that `removal`
    /// marked.
    pub fn take_marked(&mut self, removal: &Removal, at: u64, end: u64) -> Option<Region<M, R>> {
        let (start, len) = self.next_marked(removal, at, end)?;
        let (from, to) = self.window(start, start + len);
        self.relink(from, to, false);
        let region = self.map.remove(&start);
        self.relink(from, to, true);
        region
    }

    /// Ends `removal`, every region of which has been taken out.
    pub fn end_removal(&mut self, removal: Removal) {
        debug_assert!(removal.cursor >= removal.end);
        if removal.blocks {
            let range = (removal.start, removal.end);
            let at = self.removing.iter().position(|&r| r == range);
            self.removing
                .swap_remove(at.expect("the removal was recorded"));
        }
    }

    /// Removes every part of a region inside the range from `start` to
    /// `end` at once, as removal `id` would, and returns the parts.
    pub fn remove(
        &mut self,
        start: u64,
        end: u64,
        id: u64,
        budget: Option<&Arc<Budget>>,
    ) -> Result<Vec<Region<M, R>>, Error> {
        let mut parts = Vec::new();
        parts
            .try_reserve(self.map.len() + 2)
            .map_err(|_| Error::NoMemory)?;
        let mut removal = self.begin_removal(start, end, id, budget)?;
        while let Some((from, to)) = self.mark_some(&mut removal, usize::MAX) {
            while let Some(part) = self.take_marked(&removal, from, to) {
                parts.push(part);
            }
        }
        self.end_removal(removal);
        Ok(parts)
    }

    /// Checks the regions from `at` to `end`, looking at up to `limit` of
    /// them: each page must be in a region that is not being removed, and
    /// `refused` is set when `ok` refuses one. Returns where to go on, or
    /// `None` once the range is checked. Fails with `NotFound` at a page
    /// in no region.
    pub fn check_range(
        &self,
        mut at: u64,
        end: u64,
        limit: usize,
        ok: impl Fn(&Region<M, R>) -> bool,
        refused: &mut bool,
    ) -> Result<Option<u64>, Error> {
        for _ in 0..limit {
            if at >= end {
                return Ok(None);
            }
            let r = self.find(at).ok_or(Error::NotFound)?;
            *refused |= !ok(r);
            at = r.end();
        }
        Ok((at < end).then_some(at))
    }

    /// Checks the whole range from `start` to `end` at once, as
    /// `check_range` does, failing with `AccessDenied` when `ok` refuses
    /// a region and every page is in one.
    fn check_all(
        &self,
        start: u64,
        end: u64,
        ok: impl Fn(&Region<M, R>) -> bool,
    ) -> Result<(), Error> {
        let mut refused = false;
        self.check_range(start, end, usize::MAX, ok, &mut refused)?;
        if refused {
            return Err(Error::AccessDenied);
        }
        Ok(())
    }

    /// Splits the regions that extend beyond either end of the range from
    /// `start` to `end`, charging the regions that adds to `budget`, if
    /// there is one. Fails with `NoMemory` without room for the splits and
    /// with `NoResources` when the budget refuses the charge, in each case
    /// changing nothing.
    pub fn split_range(
        &mut self,
        start: u64,
        end: u64,
        budget: Option<&Arc<Budget>>,
    ) -> Result<(), Error> {
        let splits = self.charge_splits(start, end, budget)?;
        if splits.iter().any(Option::is_some) {
            let _claim = self.claim_splits()?;
            self.split_ends(start, end, splits);
        }
        Ok(())
    }

    /// Calls `change` with each region from `at` to `end` that is not
    /// being removed, looking at up to `limit` regions. Returns where to
    /// go on, or `None` once the range is done. The regions that extend
    /// beyond the range must have been split.
    pub fn change_some(
        &mut self,
        at: u64,
        end: u64,
        limit: usize,
        mut change: impl FnMut(&mut Region<M, R>),
    ) -> Option<u64> {
        if at >= end {
            return None;
        }
        for (seen, r) in self.map.range_mut(at..end).map(|(_, r)| r).enumerate() {
            if seen == limit {
                return Some(r.start);
            }
            if r.removal == 0 {
                change(r);
            }
        }
        None
    }

    /// Sets the inheritance of every page from `start` to `end` at once,
    /// splitting regions that extend beyond the range. Fails as
    /// `check_range` and `split_range` do, in each case changing nothing.
    pub fn set_inheritance(
        &mut self,
        start: u64,
        end: u64,
        inheritance: u32,
        budget: Option<&Arc<Budget>>,
    ) -> Result<(), Error> {
        self.check_all(start, end, |_| true)?;
        self.split_range(start, end, budget)?;
        self.change_some(start, end, usize::MAX, |r| r.inheritance = inheritance);
        Ok(())
    }

    /// Sets the permissions of every page from `start` to `end` at once,
    /// splitting regions that extend beyond the range. Fails as
    /// `check_range` and `split_range` do, a region refusing when `perms`
    /// goes beyond its maximum or `allowed` refuses it, in each case
    /// changing nothing.
    pub fn set_perms(
        &mut self,
        start: u64,
        end: u64,
        perms: u32,
        allowed: impl Fn(&Region<M, R>) -> bool,
        budget: Option<&Arc<Budget>>,
    ) -> Result<(), Error> {
        let ok = |r: &Region<M, R>| perms & !r.max == 0 && allowed(r);
        self.check_all(start, end, ok)?;
        self.split_range(start, end, budget)?;
        self.change_some(start, end, usize::MAX, |r| r.set_perms(perms));
        Ok(())
    }

    /// The regions that overlap the range from `start` to `end`, also
    /// those being removed, in address order.
    pub fn overlapping(&self, start: u64, end: u64) -> impl Iterator<Item = &Region<M, R>> {
        let from = self.find_any(start).map_or(start, |r| r.start);
        self.map.range(from..end.max(from)).map(|(_, r)| r)
    }

    /// The regions in address order.
    pub fn iter(&self) -> impl Iterator<Item = &Region<M, R>> {
        self.map.values()
    }

    /// Chooses the start of a range of `len` bytes, a multiple of the page
    /// size, uniformly from the page-aligned addresses at which it fits
    /// into free space of the range, and with `below_line` lies entirely
    /// below the line. `random` returns uniform random 64-bit values.
    /// Fails with `NoMemory` when there are none.
    pub fn choose(
        &self,
        len: u64,
        below_line: bool,
        mut random: impl FnMut() -> Result<u64, Error>,
    ) -> Result<u64, Error> {
        if len == 0 || !len.is_multiple_of(PAGE_SIZE) {
            return Err(Error::InvalidArgs);
        }
        let total = self.places(len / PAGE_SIZE, below_line);
        if total == 0 {
            return Err(Error::NoMemory);
        }
        // Rejection sampling keeps the choice uniform.
        let limit = u64::MAX - u64::MAX % total;
        let r = loop {
            let r = random()?;
            if r < limit {
                break r % total;
            }
        };
        Ok(self.place(len / PAGE_SIZE, below_line, r) * PAGE_SIZE)
    }

    /// The number of starts at which `n` pages fit, as `choose` counts
    /// them: those below the line, those above it, and those across it.
    fn places(&self, n: u64, below_line: bool) -> u64 {
        let below = self.below.places(n);
        if below_line {
            return below;
        }
        let across = self.across(n).map_or(0, |(from, to)| to - from);
        below + self.above.places(n) + across
    }

    /// Start `r` of those `places` counts, in pages.
    fn place(&self, n: u64, below_line: bool, mut r: u64) -> u64 {
        let below = self.below.places(n);
        if r < below || below_line {
            return self.below.place(n, r);
        }
        r -= below;
        let above = self.above.places(n);
        if r < above {
            return self.above.place(n, r);
        }
        r -= above;
        let (from, _) = self.across(n).expect("a start across the line");
        from + r
    }

    /// The starts, in pages, from the first to the second, exclusive, at
    /// which `n` pages fit into the gap across the line and cross it.
    fn across(&self, n: u64) -> Option<(u64, u64)> {
        if self.line <= self.low || self.line >= self.high || self.find_any(self.line).is_some() {
            return None;
        }
        let (a, b) = self.window(self.line, self.line);
        let (a, b, line) = (a / PAGE_SIZE, b / PAGE_SIZE, self.line / PAGE_SIZE);
        // A start `p` crosses when `p < line < p + n` and fits when
        // `a <= p` and `p + n <= b`.
        let from = a.max((line + 1).saturating_sub(n));
        let to = line.min((b + 1).saturating_sub(n));
        (from < to).then_some((from, to))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: u64 = PAGE_SIZE;

    fn region(start: u64, len: u64, offset: u64) -> Region<u8> {
        Region {
            start,
            len,
            offset,
            perms: 1,
            max: PERM_READ | PERM_WRITE,
            inheritance: 0,
            mem: 7,
            removal: 0,
            charge: Charge::NONE,
            record: (),
        }
    }

    /// No regions from 0 to 2^40, with the line at 64 pages.
    fn empty() -> Regions<u8> {
        Regions::new(0, 64 * P, 1 << 40).unwrap()
    }

    fn starts(r: &Regions<u8>) -> std::vec::Vec<(u64, u64, u64)> {
        r.iter().map(|r| (r.start, r.len, r.offset)).collect()
    }

    #[test]
    fn lists_overlapping_regions() {
        let mut r = empty();
        r.insert(region(P, 2 * P, 0)).unwrap();
        r.insert(region(4 * P, P, 0)).unwrap();
        r.insert(region(6 * P, P, 0)).unwrap();
        let at = |start, end| -> std::vec::Vec<u64> {
            r.overlapping(start, end).map(|r| r.start).collect()
        };
        assert_eq!(at(2 * P, 5 * P), [P, 4 * P]);
        assert_eq!(at(0, P), [0u64; 0]);
        assert_eq!(at(3 * P, 4 * P), [0u64; 0]);
        assert_eq!(at(4 * P + 1, 8 * P), [4 * P, 6 * P]);
        assert_eq!(at(5 * P, 2 * P), [0u64; 0]);
    }

    #[test]
    fn sets_permissions_within_the_maximum() {
        let mut r = empty();
        r.insert(region(0, 4 * P, 0)).unwrap();
        r.insert(region(6 * P, P, 0)).unwrap();
        assert_eq!(
            r.set_perms(3 * P, 7 * P, 3, |_| true, None),
            Err(Error::NotFound)
        );
        assert_eq!(
            r.set_perms(P, 2 * P, 5, |_| true, None),
            Err(Error::AccessDenied)
        );
        assert_eq!(
            r.set_perms(P, 2 * P, 3, |_| false, None),
            Err(Error::AccessDenied)
        );
        assert_eq!(starts(&r).len(), 2);
        r.set_perms(P, 2 * P, 3, |_| true, None).unwrap();
        let perms: std::vec::Vec<u32> = r.iter().map(|r| r.perms).collect();
        assert_eq!(perms, [1, 3, 1, 1]);
        assert_eq!(starts(&r)[1], (P, P, P));
    }

    #[test]
    fn charges_regions_that_splits_add() {
        use crate::object::budget::{Budget, UNLIMITED};
        let root = Budget::root();
        let budget = Budget::child(&root, 2 * REGION_COST, UNLIMITED, None).unwrap();
        let charged = |start, len| Region {
            charge: Charge::new(Some(&budget)).unwrap(),
            ..region(start, len, 0)
        };
        let mut r = empty();
        r.insert(charged(0, 8 * P)).unwrap();
        // Two splits need two more charges, which the budget refuses.
        assert_eq!(
            r.set_perms(P, 2 * P, 3, |_| true, Some(&budget)),
            Err(Error::NoResources)
        );
        assert_eq!(starts(&r).len(), 1);
        // One split fits.
        r.set_inheritance(0, P, 1, Some(&budget)).unwrap();
        assert_eq!(starts(&r).len(), 2);
        assert_eq!(budget.charge_kernel(1), Err(Error::NoResources));
        // Removing the parts releases their charges.
        drop(r.remove(0, 8 * P, 1, None).unwrap());
        budget.charge_kernel(2 * REGION_COST).unwrap();
    }

    #[test]
    fn parses_protection_and_allowed_permissions() {
        assert_eq!(MapOptions::parse_protection(1, false), Ok(1));
        assert_eq!(MapOptions::parse_protection(0, false), Ok(0));
        for bad in [2, 4, 7, 1 << 8] {
            assert_eq!(
                MapOptions::parse_protection(bad, false),
                Err(Error::InvalidArgs)
            );
        }
        assert_eq!(MapOptions::parse_protection(7, true), Ok(7));
        assert_eq!(
            MapOptions::parse_protection(6, true),
            Err(Error::InvalidArgs)
        );
        assert_eq!(MapOptions::allowed_by(rights::READ | rights::EXECUTE), 5);
        assert_eq!(MapOptions::allowed_by(rights::MAP), 0);
    }

    #[test]
    fn checks_map_options() {
        let ok = |o, a| MapOptions::parse(o, a, false).unwrap();
        let bad = |o, a| MapOptions::parse(o, a, false) == Err(Error::InvalidArgs);
        assert_eq!(ok(0, 0).perms, 0);
        assert_eq!(
            ok(PERM_READ | PERM_EXECUTE, 0).rights(),
            rights::READ | rights::EXECUTE
        );
        assert!(bad(PERM_WRITE, 0) && bad(PERM_EXECUTE, 0));
        assert!(bad(PERMS, 0) && bad(1 << 3, 0) && bad(1 << 12, 0));
        assert!(bad(0, P) && bad(SPECIFIC, P + 1) && bad(SPECIFIC | BELOW_4G, P));
        assert!(ok(SPECIFIC, P).specific && ok(BELOW_4G, 0).below_4g);
        assert_eq!(ok(2 << 9, 0).inheritance, 2);
        assert!(bad(3 << 9, 0));
        assert_eq!(MapOptions::parse(PERMS, 0, true).unwrap().perms, PERMS);
    }

    #[test]
    fn rejects_overlaps() {
        let mut r = empty();
        r.insert(region(4 * P, 2 * P, 0)).unwrap();
        assert_eq!(r.insert(region(5 * P, P, 0)), Err(Error::AlreadyExists));
        assert_eq!(r.insert(region(3 * P, 2 * P, 0)), Err(Error::AlreadyExists));
        assert_eq!(r.insert(region(3 * P, 4 * P, 0)), Err(Error::AlreadyExists));
        r.insert(region(6 * P, P, 0)).unwrap();
        r.insert(region(3 * P, P, 0)).unwrap();
        assert_eq!(r.find(6 * P + 5).unwrap().start, 6 * P);
        assert!(r.find(7 * P).is_none());
    }

    #[test]
    fn splits_regions_it_removes_from() {
        let mut r = empty();
        r.insert(region(2 * P, 4 * P, 10 * P)).unwrap();
        r.insert(region(8 * P, 2 * P, 0)).unwrap();
        let removed = r.remove(3 * P, 9 * P, 1, None).unwrap();
        assert_eq!(
            removed
                .iter()
                .map(|r| (r.start, r.len, r.offset))
                .collect::<Vec<_>>(),
            [(3 * P, 3 * P, 11 * P), (8 * P, P, 0)]
        );
        assert_eq!(starts(&r), [(2 * P, P, 10 * P), (9 * P, P, P)]);
        assert!(r.remove(20 * P, 30 * P, 1, None).unwrap().is_empty());
        assert_eq!(r.find(9 * P).unwrap().page(9 * P + 1), 1);
    }

    #[test]
    fn hides_regions_being_removed() {
        let mut r = empty();
        r.insert(region(2 * P, 4 * P, 0)).unwrap();
        let mut first = r.begin_removal(3 * P, 5 * P, 1, None).unwrap();
        assert_eq!(r.mark_some(&mut first, usize::MAX), Some((3 * P, 5 * P)));
        assert_eq!(r.mark_some(&mut first, usize::MAX), None);
        // The part being removed keeps its range but cannot be found,
        // changed or mapped over; the rest of the region stays.
        assert!(r.find(3 * P).is_none() && r.find(2 * P).is_some());
        assert!(r.overlaps(4 * P, P));
        assert_eq!(
            r.set_perms(3 * P, 4 * P, 0, |_| true, None),
            Err(Error::NotFound)
        );
        assert_eq!(
            r.set_inheritance(3 * P, 4 * P, 1, None),
            Err(Error::NotFound)
        );
        // Inserts into the range wait until the removal ends.
        assert_eq!(r.insert(region(4 * P, P, 0)), Err(Error::ShouldWait));
        // Another removal leaves it to the first.
        let other = r.remove(2 * P, 6 * P, 2, None).unwrap();
        assert_eq!(starts_of(&other), [(2 * P, P, 0), (5 * P, P, 3 * P)]);
        assert_eq!(starts(&r), [(3 * P, 2 * P, P)]);
        assert_eq!(r.next_marked(&first, 3 * P, 5 * P), Some((3 * P, 2 * P)));
        let part = r.take_marked(&first, 3 * P, 5 * P).unwrap();
        assert_eq!(starts_of(&[part]), [(3 * P, 2 * P, P)]);
        assert!(r.take_marked(&first, 3 * P, 5 * P).is_none());
        r.end_removal(first);
        assert!(starts(&r).is_empty());
        r.check_gaps();
        r.insert(region(4 * P, P, 0)).unwrap();
    }

    #[test]
    fn removes_and_changes_in_steps() {
        let mut r = empty();
        for i in 0..5 {
            r.insert(region(2 * i * P, P, 0)).unwrap();
        }
        // Steps of two regions each stop at the start of the next one.
        let mut refused = false;
        assert_eq!(r.check_range(0, P, 2, |_| false, &mut refused), Ok(None));
        assert!(refused);
        assert_eq!(
            r.check_range(0, 3 * P, 2, |_| true, &mut refused),
            Err(Error::NotFound)
        );
        assert_eq!(
            r.change_some(0, 10 * P, 2, |r| r.inheritance = 1),
            Some(4 * P)
        );
        assert_eq!(
            r.change_some(4 * P, 10 * P, 2, |r| r.inheritance = 1),
            Some(8 * P)
        );
        assert_eq!(r.change_some(8 * P, 10 * P, 2, |r| r.inheritance = 1), None);
        assert!(r.iter().all(|r| r.inheritance == 1));
        let mut removal = r.begin_removal(P, 10 * P, 1, None).unwrap();
        let mut steps = vec![];
        while let Some((from, to)) = r.mark_some(&mut removal, 2) {
            let mut taken = 0;
            while r.take_marked(&removal, from, to).is_some() {
                taken += 1;
            }
            steps.push((from, to, taken));
            r.check_gaps();
        }
        r.end_removal(removal);
        assert_eq!(steps, [(P, 6 * P, 2), (6 * P, 10 * P, 2)]);
        assert_eq!(starts(&r), [(0, P, 0)]);
    }

    fn starts_of(parts: &[Region<u8>]) -> std::vec::Vec<(u64, u64, u64)> {
        parts.iter().map(|r| (r.start, r.len, r.offset)).collect()
    }

    #[test]
    fn sets_inheritance_only_on_mapped_ranges() {
        let mut r = empty();
        r.insert(region(2 * P, 2 * P, 0)).unwrap();
        r.insert(region(4 * P, 2 * P, 0)).unwrap();
        assert_eq!(r.set_inheritance(P, 3 * P, 1, None), Err(Error::NotFound));
        assert_eq!(
            r.set_inheritance(5 * P, 7 * P, 1, None),
            Err(Error::NotFound)
        );
        assert_eq!(starts(&r), [(2 * P, 2 * P, 0), (4 * P, 2 * P, 0)]);
        r.set_inheritance(3 * P, 5 * P, 2, None).unwrap();
        let got: Vec<_> = r.iter().map(|r| (r.start, r.len, r.inheritance)).collect();
        assert_eq!(
            got,
            [(2 * P, P, 0), (3 * P, P, 2), (4 * P, P, 2), (5 * P, P, 0)]
        );
    }

    /// A record that counts the live records and the writable ones.
    struct Counted(std::rc::Rc<std::cell::Cell<(i32, i32)>>, bool);

    impl Record for Counted {
        fn split(&self, _: u64) -> Result<Counted, Error> {
            let (live, writable) = self.0.get();
            self.0.set((live + 1, writable + i32::from(self.1)));
            Ok(Counted(self.0.clone(), self.1))
        }

        fn set_writable(&mut self, writable: bool) {
            let (live, count) = self.0.get();
            self.0
                .set((live, count + i32::from(writable) - i32::from(self.1)));
            self.1 = writable;
        }
    }

    impl Drop for Counted {
        fn drop(&mut self) {
            let (live, writable) = self.0.get();
            self.0.set((live - 1, writable - i32::from(self.1)));
        }
    }

    #[test]
    fn keeps_a_record_for_each_region() {
        let counts = std::rc::Rc::new(std::cell::Cell::new((1, 0)));
        let mut r: Regions<u8, Counted> = Regions::new(0, 64 * P, 1 << 40).unwrap();
        let region = region(2 * P, 6 * P, 0);
        r.insert(Region {
            start: region.start,
            len: region.len,
            offset: region.offset,
            perms: region.perms,
            max: region.max,
            inheritance: region.inheritance,
            mem: region.mem,
            removal: 0,
            charge: Charge::NONE,
            record: Counted(counts.clone(), false),
        })
        .unwrap();
        // Splitting at both ends of the middle adds two records, and the
        // middle becomes writable.
        r.set_perms(4 * P, 6 * P, PERM_READ | PERM_WRITE, |_| true, None)
            .unwrap();
        assert_eq!(counts.get(), (3, 1));
        let removed = r.remove(4 * P, 8 * P, 1, None).unwrap();
        assert_eq!(removed.len(), 2);
        drop(removed);
        assert_eq!(counts.get(), (1, 0));
        drop(r.remove(0, 1 << 40, 2, None).unwrap());
        assert_eq!(counts.get(), (0, 0));
    }

    #[test]
    fn chooses_only_free_ranges_that_fit() {
        // Gaps from P to 2P, 3P to 5P across the line at 4P, and 6P to 8P.
        let mut r = Regions::new(P, 4 * P, 8 * P).unwrap();
        r.insert(region(2 * P, P, 0)).unwrap();
        r.insert(region(5 * P, P, 0)).unwrap();
        r.check_gaps();
        let all = |r: &Regions<u8>, len: u64, below: bool| {
            let total = r.places(len / P, below);
            let mut got: std::vec::Vec<_> = (0..total)
                .map(|i| r.choose(len, below, || Ok(i)).unwrap())
                .collect();
            got.sort_unstable();
            got
        };
        assert_eq!(all(&r, 2 * P, false), [3 * P, 6 * P]);
        assert_eq!(all(&r, P, false), [P, 3 * P, 4 * P, 6 * P, 7 * P]);
        assert_eq!(all(&r, P, true), [P, 3 * P]);
        assert_eq!(r.choose(2 * P, true, || Ok(0)), Err(Error::NoMemory));
        assert_eq!(r.choose(3 * P, false, || Ok(0)), Err(Error::NoMemory));
        assert_eq!(r.choose(0, false, || Ok(0)), Err(Error::InvalidArgs));
    }

    #[test]
    fn rejects_biased_random_values() {
        let r: Regions<u8> = Regions::new(0, 3 * P, 3 * P).unwrap();
        let mut values = [u64::MAX, 5].into_iter();
        let at = r.choose(P, false, || Ok(values.next().unwrap())).unwrap();
        assert_eq!(at, 2 * P);
    }

    /// Every start at which `len` bytes fit between the regions of `r`,
    /// below the line when `below`, found by trying each page.
    fn brute_force(r: &Regions<u8>, len: u64, below: bool) -> std::vec::Vec<u64> {
        let high = if below { r.line } else { r.high };
        (r.low / P..)
            .map(|p| p * P)
            .take_while(|&at| at + len <= high)
            .filter(|&at| !r.overlaps(at, len))
            .collect()
    }

    #[test]
    fn keeps_gaps_and_choices_exact_through_changes() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut r = Regions::new(4 * P, 100 * P, 160 * P).unwrap();
        for step in 0..600 {
            let at = (4 + next() % 156) * P;
            let len = (1 + next() % 6) * P;
            match next() % 4 {
                0 | 1 => {
                    let _ = r.insert(region(at, len, 0));
                }
                2 => {
                    let _ = r.remove(at, at + len, 1, None);
                }
                _ => {
                    let _ = r.set_inheritance(at, at + len, 1, None);
                }
            }
            if step % 50 == 0 {
                let parts = r.remove(30 * P, 130 * P, 2, None).unwrap();
                assert!(
                    parts
                        .iter()
                        .all(|p| p.start >= 30 * P && p.end() <= 130 * P)
                );
            }
            r.check_gaps();
            for pages in [1, 2, 5, 40] {
                for below in [false, true] {
                    let total = r.places(pages, below);
                    let mut got: std::vec::Vec<_> =
                        (0..total).map(|i| r.place(pages, below, i) * P).collect();
                    got.sort_unstable();
                    assert_eq!(got, brute_force(&r, pages * P, below), "{pages} pages");
                }
            }
        }
        r.remove(4 * P, 160 * P, 3, None).unwrap();
        r.check_gaps();
        assert_eq!(r.places(156, false), 1);
    }
}
