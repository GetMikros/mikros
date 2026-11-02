//! Building the x86_64 page tables the kernel starts with.

use crate::elf::{Kernel, PF_W, PF_X};
use crate::load::Layout;
use boot_protocol::{Kind, MemoryMapEntry, PAGE_SIZE};
use core::fmt;

const PRESENT: u64 = 1;
const WRITABLE: u64 = 1 << 1;
const HUGE: u64 = 1 << 7;
const NO_EXECUTE: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

const MIB_2: u64 = 2 << 20;
const GIB_1: u64 = 1 << 30;

/// The size of the boot stack.
pub const BOOT_STACK_SIZE: u64 = 64 << 10;

/// Physical page frames for page tables. Tables are reached through their
/// physical address.
pub trait Frames {
    /// A free frame, which need not be zeroed.
    fn alloc(&mut self) -> Option<u64>;
    /// The table in the frame at `phys`.
    fn table(&mut self, phys: u64) -> &mut [u64; 512];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No frame was left for a table.
    OutOfFrames,
    /// The page at this virtual address was already mapped.
    AlreadyMapped(u64),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::OutOfFrames => write!(f, "no memory was left for page tables"),
            Error::AlreadyMapped(virt) => write!(f, "the page at {virt:#x} was mapped twice"),
        }
    }
}

/// Access rights of a mapping. Every mapping is readable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    pub writable: bool,
    pub executable: bool,
}

impl Access {
    const DATA: Access = Access {
        writable: true,
        executable: false,
    };

    fn bits(self) -> u64 {
        let mut bits = PRESENT;
        if self.writable {
            bits |= WRITABLE;
        }
        if !self.executable {
            bits |= NO_EXECUTE;
        }
        bits
    }
}

fn index(virt: u64, level: u32) -> usize {
    ((virt >> (12 + 9 * level)) & 511) as usize
}

/// Builds a four-level page table hierarchy.
pub struct Mapper<'f, F: Frames> {
    frames: &'f mut F,
    root: u64,
    gib_pages: bool,
}

impl<'f, F: Frames> Mapper<'f, F> {
    /// Starts an empty hierarchy. 1 GiB pages are used only when
    /// `gib_pages` is set.
    pub fn new(frames: &'f mut F, gib_pages: bool) -> Result<Self, Error> {
        let root = Self::new_table(frames)?;
        Ok(Mapper {
            frames,
            root,
            gib_pages,
        })
    }

    /// The physical address of the top-level table.
    pub fn root(&self) -> u64 {
        self.root
    }

    fn new_table(frames: &mut F) -> Result<u64, Error> {
        let phys = frames.alloc().ok_or(Error::OutOfFrames)?;
        frames.table(phys).fill(0);
        Ok(phys)
    }

    /// Maps one page of the size that `level` selects: 0 for 4 KiB, 1 for
    /// 2 MiB and 2 for 1 GiB.
    fn map_page(&mut self, virt: u64, phys: u64, level: u32, bits: u64) -> Result<(), Error> {
        let mut table = self.root;
        for upper in (level + 1..=3).rev() {
            let i = index(virt, upper);
            let entry = self.frames.table(table)[i];
            table = if entry & PRESENT == 0 {
                let next = Self::new_table(self.frames)?;
                self.frames.table(table)[i] = next | PRESENT | WRITABLE;
                next
            } else if entry & HUGE != 0 {
                return Err(Error::AlreadyMapped(virt));
            } else {
                entry & ADDR_MASK
            };
        }
        let entry = &mut self.frames.table(table)[index(virt, level)];
        if *entry & PRESENT != 0 {
            return Err(Error::AlreadyMapped(virt));
        }
        *entry = phys | bits | if level > 0 { HUGE } else { 0 };
        Ok(())
    }

    /// Maps `size` bytes from `virt` to `phys`, using the largest pages
    /// that the alignment of both addresses allows. All three must be
    /// multiples of the page size.
    pub fn map(&mut self, virt: u64, phys: u64, size: u64, access: Access) -> Result<(), Error> {
        let bits = access.bits();
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
            self.map_page(v, p, level, bits)?;
            done += page;
        }
        Ok(())
    }
}

/// Where the parts the kernel starts with were placed.
pub struct Placement<'a> {
    pub kernel: &'a Kernel,
    pub layout: Layout,
    pub kernel_phys: u64,
    /// The physical address of the `BOOT_STACK_SIZE` bytes of boot stack.
    pub stack_phys: u64,
    pub memory_map: &'a [MemoryMapEntry],
    pub direct_map_base: u64,
    /// The bootloader pages that switch to the new tables, as a physical
    /// start and size.
    pub switch_code: (u64, u64),
}

/// The lowest virtual address of the boot stack.
pub fn stack_virt(layout: Layout) -> u64 {
    layout.virt_base - BOOT_STACK_SIZE
}

/// Builds the page tables for `placement` and returns the physical address
/// of the top-level table.
pub fn build<F: Frames>(
    frames: &mut F,
    gib_pages: bool,
    placement: &Placement,
) -> Result<u64, Error> {
    let mut mapper = Mapper::new(frames, gib_pages)?;

    let layout = placement.layout;
    for segment in &placement.kernel.segments {
        let (start, end) = segment.pages();
        let phys = placement.kernel_phys + (start as u64 - layout.virt_base);
        let access = Access {
            writable: segment.flags & PF_W != 0,
            executable: segment.flags & PF_X != 0,
        };
        mapper.map(start as u64, phys, (end - start) as u64, access)?;
    }

    mapper.map(
        stack_virt(layout),
        placement.stack_phys,
        BOOT_STACK_SIZE,
        Access::DATA,
    )?;

    // Runs of adjacent direct-mapped ranges are mapped together so that
    // large pages can span range boundaries.
    let mut run: Option<(u64, u64)> = None;
    let direct = placement
        .memory_map
        .iter()
        .filter(|e| Kind::from_u32(e.kind).is_some_and(Kind::in_direct_map));
    for entry in direct {
        let end = entry.phys_start + entry.page_count * PAGE_SIZE;
        run = match run {
            Some((start, run_end)) if run_end == entry.phys_start => Some((start, end)),
            Some((start, run_end)) => {
                let base = placement.direct_map_base;
                mapper.map(base + start, start, run_end - start, Access::DATA)?;
                Some((entry.phys_start, end))
            }
            None => Some((entry.phys_start, end)),
        };
    }
    if let Some((start, end)) = run {
        let base = placement.direct_map_base;
        mapper.map(base + start, start, end - start, Access::DATA)?;
    }

    let (code, size) = placement.switch_code;
    let access = Access {
        writable: false,
        executable: true,
    };
    mapper.map(code, code, size, access)?;

    Ok(mapper.root())
}

#[cfg(test)]
mod tests;
