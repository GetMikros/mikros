//! The regions of a user address space: which range maps which part of
//! which memory object, and with what permissions.

use crate::object::rights;
use crate::status::Error;
use alloc::collections::BTreeMap;
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
    /// that are writable and executable are refused, as no process can
    /// have a policy that allows them yet.
    pub fn parse(options: u32, addr: u64) -> Result<MapOptions, Error> {
        let perms = options & PERMS;
        let inheritance = (options & INHERITANCE_MASK) >> INHERITANCE_SHIFT;
        let specific = options & SPECIFIC != 0;
        let below_4g = options & BELOW_4G != 0;
        let unreadable = perms & (PERM_WRITE | PERM_EXECUTE) != 0 && perms & PERM_READ == 0;
        let write_execute = perms & PERM_WRITE != 0 && perms & PERM_EXECUTE != 0;
        let bad_addr = if specific {
            !addr.is_multiple_of(PAGE_SIZE)
        } else {
            addr != 0
        };
        if options & !DEFINED != 0
            || unreadable
            || write_execute
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
    /// and executable.
    pub fn parse_protection(options: u32) -> Result<u32, Error> {
        let unreadable = options & (PERM_WRITE | PERM_EXECUTE) != 0 && options & PERM_READ == 0;
        let write_execute = options & PERM_WRITE != 0 && options & PERM_EXECUTE != 0;
        if options & !PERMS != 0 || unreadable || write_execute {
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

/// A mapped range. `M` is the memory object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region<M> {
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
}

impl<M> Region<M> {
    pub fn end(&self) -> u64 {
        self.start + self.len
    }

    /// The page index in the object of the page at `addr`, which lies in
    /// the region.
    pub fn page(&self, addr: u64) -> u64 {
        (self.offset + (addr - self.start)) / PAGE_SIZE
    }
}

/// Non-overlapping regions, keyed by their start.
pub struct Regions<M> {
    map: BTreeMap<u64, Region<M>>,
}

impl<M> Default for Regions<M> {
    fn default() -> Self {
        Regions {
            map: BTreeMap::new(),
        }
    }
}

impl<M: Clone> Regions<M> {
    /// The region containing `addr`.
    pub fn find(&self, addr: u64) -> Option<&Region<M>> {
        let (_, r) = self.map.range(..=addr).next_back()?;
        (addr < r.end()).then_some(r)
    }

    /// Whether any region overlaps the `len` bytes from `start`.
    pub fn overlaps(&self, start: u64, len: u64) -> bool {
        let end = start + len;
        self.find(start).is_some() || self.map.range(start..end).next().is_some()
    }

    /// Adds `region`, or fails with `AlreadyExists` when it overlaps one.
    pub fn insert(&mut self, region: Region<M>) -> Result<(), Error> {
        if self.overlaps(region.start, region.len) {
            return Err(Error::AlreadyExists);
        }
        self.map.insert(region.start, region);
        Ok(())
    }

    /// Splits the region containing `addr` so that one starts there.
    fn split_at(&mut self, addr: u64) {
        let Some(r) = self.find(addr) else {
            return;
        };
        let start = r.start;
        if start == addr {
            return;
        }
        let mut tail = r.clone();
        let head = self.map.get_mut(&start).expect("found above");
        head.len = addr - head.start;
        tail.offset += head.len;
        tail.len -= head.len;
        tail.start = addr;
        self.map.insert(addr, tail);
    }

    /// Removes every part of a region inside the range from `start` to
    /// `end` and returns the removed parts, splitting regions that extend
    /// beyond it.
    pub fn remove(&mut self, start: u64, end: u64) -> Vec<Region<M>> {
        self.split_at(start);
        self.split_at(end);
        let starts: Vec<u64> = self.map.range(start..end).map(|(&s, _)| s).collect();
        let mut removed = Vec::with_capacity(starts.len());
        for s in starts {
            removed.extend(self.map.remove(&s));
        }
        removed
    }

    /// Sets the inheritance of every page from `start` to `end`, splitting
    /// regions that extend beyond the range. Fails with `NotFound`, and
    /// changes nothing, when a page in the range is not mapped.
    pub fn set_inheritance(&mut self, start: u64, end: u64, inheritance: u32) -> Result<(), Error> {
        let mut at = start;
        while at < end {
            at = self.find(at).ok_or(Error::NotFound)?.end();
        }
        self.split_at(start);
        self.split_at(end);
        for r in self.map.range_mut(start..end).map(|(_, r)| r) {
            r.inheritance = inheritance;
        }
        Ok(())
    }

    /// Sets the permissions of every page from `start` to `end`,
    /// splitting regions that extend beyond the range. Fails with
    /// `NotFound` when a page in the range is not mapped and with
    /// `AccessDenied` when `perms` goes beyond a region's maximum or
    /// `allowed` refuses a region, in both cases changing nothing.
    pub fn set_perms(
        &mut self,
        start: u64,
        end: u64,
        perms: u32,
        allowed: impl Fn(&Region<M>) -> bool,
    ) -> Result<(), Error> {
        let mut at = start;
        while at < end {
            at = self.find(at).ok_or(Error::NotFound)?.end();
        }
        let mut at = start;
        while at < end {
            let r = self.find(at).expect("checked above");
            if perms & !r.max != 0 || !allowed(r) {
                return Err(Error::AccessDenied);
            }
            at = r.end();
        }
        self.split_at(start);
        self.split_at(end);
        for r in self.map.range_mut(start..end).map(|(_, r)| r) {
            r.perms = perms;
        }
        Ok(())
    }

    /// The regions in address order.
    pub fn iter(&self) -> impl Iterator<Item = &Region<M>> {
        self.map.values()
    }

    /// Chooses the start of a range of `len` bytes, a multiple of the page
    /// size, uniformly from the page-aligned addresses from `low` to
    /// `high` at which it fits into free space. `random` returns uniform
    /// random 64-bit values. Fails with `NoMemory` when there are none.
    pub fn choose(
        &self,
        len: u64,
        low: u64,
        high: u64,
        mut random: impl FnMut() -> Result<u64, Error>,
    ) -> Result<u64, Error> {
        let pages = len / PAGE_SIZE;
        // The number of starts in each gap, lowest gap first.
        let gaps = || {
            let mut at = low;
            let mut bounds = self.map.values().map(|r| (r.start, r.end()));
            core::iter::from_fn(move || {
                while at < high {
                    let (next, after) = bounds
                        .find(|&(_, end)| end > at)
                        .map_or((high, high), |(s, e)| (s.clamp(at, high), e));
                    let gap = (next - at) / PAGE_SIZE;
                    let from = at;
                    at = after.max(next);
                    if gap >= pages {
                        return Some((from, gap - pages + 1));
                    }
                }
                None
            })
        };
        let total: u64 = gaps().map(|(_, n)| n).sum();
        if total == 0 {
            return Err(Error::NoMemory);
        }
        // Rejection sampling keeps the choice uniform.
        let limit = u64::MAX - u64::MAX % total;
        let mut pick = loop {
            let r = random()?;
            if r < limit {
                break r % total;
            }
        };
        for (from, n) in gaps() {
            if pick < n {
                return Ok(from + pick * PAGE_SIZE);
            }
            pick -= n;
        }
        unreachable!("the pick is below the total")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

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
        }
    }

    fn starts(r: &Regions<u8>) -> std::vec::Vec<(u64, u64, u64)> {
        r.iter().map(|r| (r.start, r.len, r.offset)).collect()
    }

    #[test]
    fn sets_permissions_within_the_maximum() {
        let mut r = Regions::default();
        r.insert(region(0, 4 * P, 0)).unwrap();
        r.insert(region(6 * P, P, 0)).unwrap();
        assert_eq!(r.set_perms(3 * P, 7 * P, 3, |_| true), Err(Error::NotFound));
        assert_eq!(r.set_perms(P, 2 * P, 5, |_| true), Err(Error::AccessDenied));
        assert_eq!(
            r.set_perms(P, 2 * P, 3, |_| false),
            Err(Error::AccessDenied)
        );
        assert_eq!(starts(&r).len(), 2);
        r.set_perms(P, 2 * P, 3, |_| true).unwrap();
        let perms: std::vec::Vec<u32> = r.iter().map(|r| r.perms).collect();
        assert_eq!(perms, [1, 3, 1, 1]);
        assert_eq!(starts(&r)[1], (P, P, P));
    }

    #[test]
    fn parses_protection_and_allowed_permissions() {
        assert_eq!(MapOptions::parse_protection(1), Ok(1));
        assert_eq!(MapOptions::parse_protection(0), Ok(0));
        for bad in [2, 4, 7, 1 << 8] {
            assert_eq!(MapOptions::parse_protection(bad), Err(Error::InvalidArgs));
        }
        assert_eq!(MapOptions::allowed_by(rights::READ | rights::EXECUTE), 5);
        assert_eq!(MapOptions::allowed_by(rights::MAP), 0);
    }

    #[test]
    fn checks_map_options() {
        let ok = |o, a| MapOptions::parse(o, a).unwrap();
        let bad = |o, a| MapOptions::parse(o, a) == Err(Error::InvalidArgs);
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
    }

    #[test]
    fn rejects_overlaps() {
        let mut r = Regions::default();
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
        let mut r = Regions::default();
        r.insert(region(2 * P, 4 * P, 10 * P)).unwrap();
        r.insert(region(8 * P, 2 * P, 0)).unwrap();
        let removed = r.remove(3 * P, 9 * P);
        assert_eq!(
            removed
                .iter()
                .map(|r| (r.start, r.len, r.offset))
                .collect::<Vec<_>>(),
            [(3 * P, 3 * P, 11 * P), (8 * P, P, 0)]
        );
        assert_eq!(starts(&r), [(2 * P, P, 10 * P), (9 * P, P, P)]);
        assert!(r.remove(20 * P, 30 * P).is_empty());
        assert_eq!(r.find(9 * P).unwrap().page(9 * P + 1), 1);
    }

    #[test]
    fn sets_inheritance_only_on_mapped_ranges() {
        let mut r = Regions::default();
        r.insert(region(2 * P, 2 * P, 0)).unwrap();
        r.insert(region(4 * P, 2 * P, 0)).unwrap();
        assert_eq!(r.set_inheritance(P, 3 * P, 1), Err(Error::NotFound));
        assert_eq!(r.set_inheritance(5 * P, 7 * P, 1), Err(Error::NotFound));
        assert_eq!(starts(&r), [(2 * P, 2 * P, 0), (4 * P, 2 * P, 0)]);
        r.set_inheritance(3 * P, 5 * P, 2).unwrap();
        let got: Vec<_> = r.iter().map(|r| (r.start, r.len, r.inheritance)).collect();
        assert_eq!(
            got,
            [(2 * P, P, 0), (3 * P, P, 2), (4 * P, P, 2), (5 * P, P, 0)]
        );
    }

    #[test]
    fn chooses_only_free_ranges_that_fit() {
        let mut r = Regions::default();
        r.insert(region(2 * P, P, 0)).unwrap();
        r.insert(region(5 * P, P, 0)).unwrap();
        // From P to 8P, two pages fit only at 3P and 6P.
        let mut seen = vec![];
        for i in 0..3 {
            seen.push(r.choose(2 * P, P, 8 * P, || Ok(i)).unwrap());
        }
        assert_eq!(seen, [3 * P, 6 * P, 3 * P]);
        assert_eq!(r.choose(3 * P, P, 8 * P, || Ok(0)), Err(Error::NoMemory));
        assert_eq!(r.choose(P, P, 8 * P, || Ok(4)).unwrap(), 7 * P);
        assert_eq!(r.choose(P, 0, 2 * P, || Ok(1)).unwrap(), P);
    }

    #[test]
    fn rejects_biased_random_values() {
        let r: Regions<u8> = Regions::default();
        let mut values = [u64::MAX, 5].into_iter();
        let at = r
            .choose(P, 0, 3 * P, || Ok(values.next().unwrap()))
            .unwrap();
        assert_eq!(at, 2 * P);
    }
}
