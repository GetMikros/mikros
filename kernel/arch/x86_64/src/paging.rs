//! x86_64 page tables.

use core::fmt;

pub const PAGE_SIZE: u64 = 4096;
const MIB_2: u64 = 2 << 20;
const GIB_1: u64 = 1 << 30;

const PRESENT: u64 = 1;
const WRITABLE: u64 = 1 << 1;
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

/// A cache policy and the PAT entry that gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "device and framebuffer mappings come later")
)]
pub enum Policy {
    Cached = 0,
    WriteCombining = 1,
    Uncached = 2,
    UncachedDevice = 3,
}

/// How a range is mapped. Every mapping is readable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags {
    pub writable: bool,
    pub executable: bool,
    /// Set for mappings in the kernel half.
    pub global: bool,
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
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::OutOfMemory => write!(f, "no memory for page tables"),
            Error::AlreadyMapped(v) => write!(f, "the page at {v:#x} is already mapped"),
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
                self.tables.table(table)[i] = next | PRESENT | WRITABLE;
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
            table = entry & ADDR_MASK;
        }
        unreachable!()
    }

    const KERNEL_DATA: Flags = Flags {
        writable: true,
        executable: false,
        global: true,
        policy: Policy::Cached,
    };

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
    }
}
