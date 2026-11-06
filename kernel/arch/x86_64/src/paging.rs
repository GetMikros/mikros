//! x86_64 page tables.

use core::fmt;

pub const PAGE_SIZE: u64 = 4096;
const MIB_2: u64 = 2 << 20;
const GIB_1: u64 = 1 << 30;

const PRESENT: u64 = 1;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const PWT: u64 = 1 << 3;
const PCD: u64 = 1 << 4;
const HUGE: u64 = 1 << 7;
const PAT_4K: u64 = 1 << 7;
const GLOBAL: u64 = 1 << 8;
const PAT_HUGE: u64 = 1 << 12;
const NO_EXECUTE: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

/// The IA32_PAT value: entries 0 to 7 are write-back, write-combining,
/// uncached-minus, uncached, write-back, write-protected, uncached-minus
/// and write-through.
pub const PAT_VALUE: u64 = u64::from_le_bytes([6, 1, 7, 0, 6, 5, 7, 4]);

/// A cache policy, whose value is the PAT entry that gives it.
pub use kernel_core::arch::CachePolicy as Policy;

/// How a range is mapped. Every mapping is readable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags {
    pub writable: bool,
    pub executable: bool,
    /// Set for mappings in the kernel half.
    pub global: bool,
    /// Set for mappings in the user address range.
    pub user: bool,
    pub policy: Policy,
}

impl Flags {
    /// The leaf entry bits, for a 4 KiB page unless `huge`.
    fn bits(self, huge: bool) -> u64 {
        let mut bits = PRESENT;
        if self.writable {
            bits |= WRITABLE;
        }
        if !self.executable {
            bits |= NO_EXECUTE;
        }
        if self.global {
            bits |= GLOBAL;
        }
        if self.user {
            bits |= USER;
        }
        let pat = self.policy as u64;
        if pat & 1 != 0 {
            bits |= PWT;
        }
        if pat & 2 != 0 {
            bits |= PCD;
        }
        if pat & 4 != 0 {
            bits |= if huge { PAT_HUGE } else { PAT_4K };
        }
        bits
    }
}

/// Page table frames, reached through their physical address.
pub trait Tables {
    /// A free frame, which need not be zeroed.
    fn alloc(&mut self) -> Option<u64>;
    /// The table in the frame at `phys`.
    fn table(&mut self, phys: u64) -> &mut [u64; 512];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
    /// The page at this virtual address is already mapped.
    AlreadyMapped(u64),
    /// The page at this virtual address is not mapped.
    NotMapped(u64),
    /// The range ends inside the large page at this virtual address.
    PartOfLargePage(u64),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::OutOfMemory => write!(f, "no memory for page tables"),
            Error::AlreadyMapped(v) => write!(f, "the page at {v:#x} is already mapped"),
            Error::NotMapped(v) => write!(f, "the page at {v:#x} is not mapped"),
            Error::PartOfLargePage(v) => {
                write!(f, "the range covers only part of the large page at {v:#x}")
            }
        }
    }
}

fn index(virt: u64, level: u32) -> usize {
    ((virt >> (12 + 9 * level)) & 511) as usize
}

/// A four-level hierarchy.
pub struct Mapper<'t, T: Tables> {
    tables: &'t mut T,
    root: u64,
    gib_pages: bool,
}

impl<'t, T: Tables> Mapper<'t, T> {
    fn new_table(tables: &mut T) -> Result<u64, Error> {
        let phys = tables.alloc().ok_or(Error::OutOfMemory)?;
        tables.table(phys).fill(0);
        Ok(phys)
    }

    /// A hierarchy whose top-level entries 256 to 511 all point to tables
    /// of their own, so that those entries never change.
    pub fn new(tables: &'t mut T, gib_pages: bool) -> Result<Self, Error> {
        let root = Self::new_table(tables)?;
        for i in 256..512 {
            let next = Self::new_table(tables)?;
            tables.table(root)[i] = next | PRESENT | WRITABLE;
        }
        Ok(Mapper {
            tables,
            root,
            gib_pages,
        })
    }

    /// A hierarchy for a user address space: an empty lower half, and the
    /// top-level entries 256 to 511 of the hierarchy at `kernel_root`.
    pub fn user(tables: &'t mut T, kernel_root: u64) -> Result<Self, Error> {
        let root = Self::new_table(tables)?;
        let kernel = *tables.table(kernel_root);
        tables.table(root)[256..].copy_from_slice(&kernel[256..]);
        Ok(Mapper {
            tables,
            root,
            gib_pages: false,
        })
    }

    /// The hierarchy whose top-level table is at `root`.
    pub fn existing(tables: &'t mut T, root: u64, gib_pages: bool) -> Self {
        Mapper {
            tables,
            root,
            gib_pages,
        }
    }

    pub fn root(&self) -> u64 {
        self.root
    }

    fn map_page(&mut self, virt: u64, phys: u64, level: u32, flags: Flags) -> Result<(), Error> {
        let mut table = self.root;
        for upper in (level + 1..=3).rev() {
            let i = index(virt, upper);
            let entry = self.tables.table(table)[i];
            table = if entry & PRESENT == 0 {
                let next = Self::new_table(self.tables)?;
                let user = if flags.user { USER } else { 0 };
                self.tables.table(table)[i] = next | PRESENT | WRITABLE | user;
                next
            } else if entry & HUGE != 0 {
                return Err(Error::AlreadyMapped(virt));
            } else {
                entry & ADDR_MASK
            };
        }
        let entry = &mut self.tables.table(table)[index(virt, level)];
        if *entry & PRESENT != 0 {
            return Err(Error::AlreadyMapped(virt));
        }
        let huge = level > 0;
        *entry = phys | flags.bits(huge) | if huge { HUGE } else { 0 };
        Ok(())
    }

    /// The table holding the leaf entry for `virt`, the entry's index and
    /// its page size, or, when `virt` is not mapped, the size of the range
    /// that the missing entry would cover.
    fn walk(&mut self, virt: u64) -> Result<(u64, usize, u64), u64> {
        let mut table = self.root;
        for level in (0..=3).rev() {
            let i = index(virt, level);
            let entry = self.tables.table(table)[i];
            let span = 1 << (12 + 9 * level);
            if entry & PRESENT == 0 {
                return Err(span);
            }
            if level == 0 || (level < 3 && entry & HUGE != 0) {
                return Ok((table, i, span));
            }
            table = entry & ADDR_MASK;
        }
        unreachable!()
    }

    /// The physical address that `virt` maps to, or `None` when it is
    /// not mapped.
    pub fn translate(&mut self, virt: u64) -> Option<u64> {
        let (table, i, span) = self.find_leaf(virt)?;
        let entry = self.tables.table(table)[i];
        Some((entry & ADDR_MASK & !(span - 1)) + (virt & (span - 1)))
    }

    /// The table holding the leaf entry for `virt`, the entry's index and
    /// its page size, or `None` when `virt` is not mapped.
    fn find_leaf(&mut self, virt: u64) -> Option<(u64, usize, u64)> {
        self.walk(virt).ok()
    }

    /// Removes the mappings of the `size` bytes from `virt` and calls
    /// `unmapped` with the virtual address, the physical address and the
    /// size of each page removed. Every page in the range must be mapped,
    /// and large pages must lie wholly inside it; otherwise nothing is
    /// changed. The caller invalidates the translations. Page tables that
    /// become empty are kept.
    pub fn unmap(
        &mut self,
        virt: u64,
        size: u64,
        mut unmapped: impl FnMut(u64, u64, u64),
    ) -> Result<(), Error> {
        for apply in [false, true] {
            let mut done = 0;
            while done < size {
                let v = virt.wrapping_add(done);
                let (table, i, page) = self.find_leaf(v).ok_or(Error::NotMapped(v))?;
                if v % page != 0 || size - done < page {
                    return Err(Error::PartOfLargePage(v));
                }
                if apply {
                    let entry = core::mem::take(&mut self.tables.table(table)[i]);
                    unmapped(v, entry & ADDR_MASK & !(page - 1), page);
                }
                done += page;
            }
        }
        Ok(())
    }

    /// Removes the mappings of the 4 KiB pages in the `size` bytes from
    /// `virt` that are mapped, skipping the others, and calls `unmapped`
    /// with the virtual and physical address of each page removed. The
    /// range must not contain large pages. The caller invalidates the
    /// translations.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the kernel removes pages in steps with `unmap_present_some`"
        )
    )]
    pub fn unmap_present(
        &mut self,
        virt: u64,
        size: u64,
        unmapped: impl FnMut(u64, u64),
    ) -> Result<(), Error> {
        self.unmap_present_some(virt, size, usize::MAX, unmapped)
            .map(drop)
    }

    /// Does what `unmap_present` does, but visits at most `limit` entries,
    /// each page removed or span skipped counting as one, and returns the
    /// address it stopped at when the range goes on.
    pub fn unmap_present_some(
        &mut self,
        virt: u64,
        size: u64,
        limit: usize,
        mut unmapped: impl FnMut(u64, u64),
    ) -> Result<Option<u64>, Error> {
        let end = virt.wrapping_add(size);
        let mut v = virt;
        let mut visited = 0;
        while v != end {
            if visited == limit {
                return Ok(Some(v));
            }
            visited += 1;
            match self.walk(v) {
                Ok((table, i, page)) => {
                    if page != PAGE_SIZE {
                        return Err(Error::PartOfLargePage(v));
                    }
                    let entry = core::mem::take(&mut self.tables.table(table)[i]);
                    unmapped(v, entry & ADDR_MASK);
                    v = v.wrapping_add(PAGE_SIZE);
                }
                // Skip the rest of the range the missing entry covers.
                Err(span) => {
                    let next = (v | (span - 1)).wrapping_add(1);
                    let left = end.wrapping_sub(v);
                    v = if next.wrapping_sub(v) >= left {
                        end
                    } else {
                        next
                    };
                }
            }
        }
        Ok(None)
    }

    /// Calls `free` with each table that top-level entries 0 to 255 lead
    /// to, lowest level first, and clears those entries. The lower half
    /// must map nothing, or only 4 KiB pages whose frames the caller
    /// otherwise keeps track of. The top-level table itself is kept.
    pub fn free_lower_half(&mut self, mut free: impl FnMut(u64)) {
        fn walk<T: Tables>(tables: &mut T, table: u64, level: u32, free: &mut impl FnMut(u64)) {
            if level > 0 {
                for i in 0..512 {
                    let entry = tables.table(table)[i];
                    if entry & PRESENT != 0 && entry & HUGE == 0 {
                        walk(tables, entry & ADDR_MASK, level - 1, free);
                    }
                }
            }
            free(table);
        }
        for i in 0..256 {
            let entry = core::mem::take(&mut self.tables.table(self.root)[i]);
            if entry & PRESENT != 0 {
                walk(self.tables, entry & ADDR_MASK, 2, &mut free);
            }
        }
    }

    /// Maps `size` bytes from `virt` to `phys` with the largest pages their
    /// alignment allows. All three are multiples of the page size.
    pub fn map(&mut self, virt: u64, phys: u64, size: u64, flags: Flags) -> Result<(), Error> {
        let mut done = 0;
        while done < size {
            let (v, p, left) = (virt.wrapping_add(done), phys + done, size - done);
            let fits = |page: u64| v % page == 0 && p % page == 0 && left >= page;
            let (level, page) = if self.gib_pages && fits(GIB_1) {
                (2, GIB_1)
            } else if fits(MIB_2) {
                (1, MIB_2)
            } else {
                (0, PAGE_SIZE)
            };
            self.map_page(v, p, level, flags)?;
            done += page;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;
    use std::vec::Vec;

    struct Pool(Vec<Box<[u64; 512]>>);

    const BASE: u64 = 0x100_0000;

    impl Tables for Pool {
        fn alloc(&mut self) -> Option<u64> {
            (self.0.len() < 400).then(|| {
                self.0.push(Box::new([0xdead; 512]));
                BASE + (self.0.len() as u64 - 1) * PAGE_SIZE
            })
        }
        fn table(&mut self, phys: u64) -> &mut [u64; 512] {
            &mut self.0[((phys - BASE) / PAGE_SIZE) as usize]
        }
    }

    /// The leaf entry and page size that map `virt`.
    fn leaf(pool: &mut Pool, root: u64, virt: u64) -> Option<(u64, u64)> {
        let mut table = root;
        for level in (0..=3).rev() {
            let entry = pool.table(table)[index(virt, level)];
            if entry & PRESENT == 0 {
                return None;
            }
            if level == 0 || entry & HUGE != 0 {
                return Some((entry, 1 << (12 + 9 * level)));
            }
            assert_eq!(entry & (NO_EXECUTE | GLOBAL), 0);
            assert_eq!(entry & USER != 0, virt < 1 << 47);
            table = entry & ADDR_MASK;
        }
        unreachable!()
    }

    const KERNEL_DATA: Flags = Flags {
        writable: true,
        executable: false,
        global: true,
        user: false,
        policy: Policy::Cached,
    };

    const USER_DATA: Flags = Flags {
        writable: true,
        executable: false,
        global: false,
        user: true,
        policy: Policy::Cached,
    };

    #[test]
    fn translates_small_and_large_pages() {
        let mut pool = Pool(Vec::new());
        let mut m = Mapper::new(&mut pool, true).unwrap();
        let small = 0xffff_c000_0000_3000;
        m.map(small, 0x7000, PAGE_SIZE, KERNEL_DATA).unwrap();
        let large = 0xffff_8000_4000_0000;
        m.map(large, 0x8000_0000, GIB_1, KERNEL_DATA).unwrap();
        assert_eq!(m.translate(small + 0x12), Some(0x7012));
        assert_eq!(m.translate(large + 0x20_3456), Some(0x8020_3456));
        assert_eq!(m.translate(small + PAGE_SIZE), None);
        assert_eq!(m.translate(0x1000), None);
    }

    #[test]
    fn programs_the_pat_entries() {
        assert_eq!(PAT_VALUE, 0x0407_0506_0007_0106);
    }

    #[test]
    fn preallocates_the_kernel_half() {
        let mut pool = Pool(Vec::new());
        let mapper = Mapper::new(&mut pool, false).unwrap();
        let root = mapper.root();
        assert_eq!(pool.0.len(), 257);
        let top = pool.table(root);
        assert!(top[..256].iter().all(|&e| e == 0));
        assert!(top[256..].iter().all(|&e| e & PRESENT != 0));
    }

    #[test]
    fn selects_the_pat_entry_of_each_policy() {
        let mut pool = Pool(Vec::new());
        let mut mapper = Mapper::new(&mut pool, true).unwrap();
        let base = 0xffff_c000_0000_0000;
        let policies = [
            Policy::Cached,
            Policy::WriteCombining,
            Policy::Uncached,
            Policy::UncachedDevice,
        ];
        for (i, policy) in policies.into_iter().enumerate() {
            let flags = Flags {
                policy,
                ..KERNEL_DATA
            };
            let at = i as u64 * PAGE_SIZE;
            mapper
                .map(base + at, 0xfed0_0000 + at, PAGE_SIZE, flags)
                .unwrap();
        }
        let root = mapper.root();
        let bits: Vec<u64> = (0..4)
            .map(|i| leaf(&mut pool, root, base + i * PAGE_SIZE).unwrap().0 & (PWT | PCD | PAT_4K))
            .collect();
        assert_eq!(bits, [0, PWT, PCD, PWT | PCD]);
        let (entry, _) = leaf(&mut pool, root, base).unwrap();
        assert_eq!(
            entry & (GLOBAL | NO_EXECUTE | WRITABLE),
            GLOBAL | NO_EXECUTE | WRITABLE
        );
        assert_eq!(entry & ADDR_MASK, 0xfed0_0000);
    }

    #[test]
    fn uses_large_pages_and_refuses_overlaps() {
        let mut pool = Pool(Vec::new());
        let mut mapper = Mapper::new(&mut pool, true).unwrap();
        let dm = 0xffff_8000_0000_0000;
        mapper
            .map(dm, 0, GIB_1 + MIB_2 + PAGE_SIZE, KERNEL_DATA)
            .unwrap();
        assert_eq!(
            mapper.map(dm + GIB_1, 0, PAGE_SIZE, KERNEL_DATA),
            Err(Error::AlreadyMapped(dm + GIB_1))
        );
        let root = mapper.root();
        let mut size = |v| leaf(&mut pool, root, v).map(|l| l.1);
        assert_eq!(size(dm), Some(GIB_1));
        assert_eq!(size(dm + GIB_1), Some(MIB_2));
        assert_eq!(size(dm + GIB_1 + MIB_2), Some(PAGE_SIZE));
        assert_eq!(size(dm + GIB_1 + MIB_2 + PAGE_SIZE), None);

        // The same hierarchy, reopened, sees the earlier mappings.
        let mut again = Mapper::existing(&mut pool, root, true);
        assert_eq!(
            again.map(dm, 0, PAGE_SIZE, KERNEL_DATA),
            Err(Error::AlreadyMapped(dm))
        );
    }

    #[test]
    fn unmaps_whole_pages_only() {
        let mut pool = Pool(Vec::new());
        let mut mapper = Mapper::new(&mut pool, true).unwrap();
        let base = 0xffff_c000_0000_0000;
        mapper
            .map(base, 0x20_0000, MIB_2 + 2 * PAGE_SIZE, KERNEL_DATA)
            .unwrap();
        let mut seen = Vec::new();
        assert_eq!(
            mapper.unmap(base, PAGE_SIZE, |v, p, size| seen.push((v, p, size))),
            Err(Error::PartOfLargePage(base))
        );
        let end = base + MIB_2 + 2 * PAGE_SIZE;
        assert_eq!(
            mapper.unmap(base, MIB_2 + 3 * PAGE_SIZE, |v, p, size| seen
                .push((v, p, size))),
            Err(Error::NotMapped(end))
        );
        assert!(seen.is_empty());
        mapper
            .unmap(base, MIB_2 + PAGE_SIZE, |v, p, size| {
                seen.push((v, p, size))
            })
            .unwrap();
        assert_eq!(
            seen,
            [
                (base, 0x20_0000, MIB_2),
                (base + MIB_2, 0x40_0000, PAGE_SIZE)
            ]
        );
        let root = mapper.root();
        assert_eq!(leaf(&mut pool, root, base), None);
        assert_eq!(leaf(&mut pool, root, base + MIB_2), None);
        assert!(leaf(&mut pool, root, base + MIB_2 + PAGE_SIZE).is_some());
        // The range can be mapped again.
        let mut mapper = Mapper::existing(&mut pool, root, true);
        mapper.map(base, 0, PAGE_SIZE, KERNEL_DATA).unwrap();
    }

    #[test]
    fn shares_the_kernel_half_with_user_spaces() {
        let mut pool = Pool(Vec::new());
        let kernel = Mapper::new(&mut pool, false).unwrap().root();
        let user = Mapper::user(&mut pool, kernel).unwrap().root();
        let k = *pool.table(kernel);
        let u = *pool.table(user);
        assert_eq!(k[256..], u[256..]);
        assert!(u[..256].iter().all(|&e| e == 0));
    }

    #[test]
    fn maps_and_frees_user_pages() {
        let mut pool = Pool(Vec::new());
        let kernel = Mapper::new(&mut pool, false).unwrap().root();
        let mut mapper = Mapper::user(&mut pool, kernel).unwrap();
        let base = 0x40_0000;
        mapper.map(base, 0x9000, PAGE_SIZE, USER_DATA).unwrap();
        mapper
            .map(base + 2 * PAGE_SIZE, 0xA000, PAGE_SIZE, USER_DATA)
            .unwrap();
        mapper
            .map(0x7FFF_FFFF_E000, 0xB000, PAGE_SIZE, USER_DATA)
            .unwrap();
        let root = mapper.root();
        let (entry, size) = leaf(&mut pool, root, base).unwrap();
        assert_eq!((entry & (USER | GLOBAL), size), (USER, PAGE_SIZE));

        let mut mapper = Mapper::existing(&mut pool, root, false);
        let mut seen = Vec::new();
        // A limited walk stops after visiting that many entries, the
        // missing page counting as one.
        let next = mapper
            .unmap_present_some(base, 3 * PAGE_SIZE, 2, |v, p| seen.push((v, p)))
            .unwrap();
        assert_eq!(next, Some(base + 2 * PAGE_SIZE));
        let next = mapper
            .unmap_present_some(base + 2 * PAGE_SIZE, PAGE_SIZE, 2, |v, p| seen.push((v, p)))
            .unwrap();
        assert_eq!(next, None);
        assert_eq!(seen, [(base, 0x9000), (base + 2 * PAGE_SIZE, 0xA000)]);
        mapper.map(base, 0x9000, PAGE_SIZE, USER_DATA).unwrap();
        mapper
            .map(base + 2 * PAGE_SIZE, 0xA000, PAGE_SIZE, USER_DATA)
            .unwrap();
        seen.clear();
        mapper
            .unmap_present(base, 3 * PAGE_SIZE, |v, p| seen.push((v, p)))
            .unwrap();
        assert_eq!(seen, [(base, 0x9000), (base + 2 * PAGE_SIZE, 0xA000)]);
        // A range as large as the user half is walked by its tables.
        seen.clear();
        mapper
            .unmap_present(0x1_0000, 0x7FFF_FFFF_F000 - 0x1_0000, |v, p| {
                seen.push((v, p))
            })
            .unwrap();
        assert_eq!(seen, [(0x7FFF_FFFF_E000, 0xB000)]);
        let mut freed = Vec::new();
        mapper.free_lower_half(|t| freed.push(t));
        // Two hierarchies of a PDPT, a directory and a page table each.
        assert_eq!(freed.len(), 6);
        freed.sort();
        freed.dedup();
        assert_eq!(freed.len(), 6);
        assert!(!freed.contains(&root) && !freed.contains(&kernel));
        assert!(pool.table(root)[..256].iter().all(|&e| e == 0));
    }
}
