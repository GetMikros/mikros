//! The heap check of the diagnostic build. A record for each page that
//! the heap gets says what it holds: blocks of one size class, the start
//! of a whole-page block and its page count, or nothing. Freed small
//! blocks hold a mark after their link and a poison pattern after that,
//! so that a block freed twice, written after its free or reached through
//! a damaged free list is found before the heap uses it.

use super::{MIN_CLASS, PAGE_SIZE, Pages};
use core::fmt;
use core::ptr::NonNull;

/// The record of the first page of a whole-page block, with its count.
const LARGE: u32 = 0x8000_0000;
/// The word after the link of a freed small block.
const FREED: u64 = 0xdead_f4ee_b10c_f4ee;
/// The words after that.
const POISON: u64 = 0x6b6b_6b6b_6b6b_6b6b;

/// A failed check, which the heap's caller reports once it no longer
/// holds the heap's lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    pub what: &'static str,
    pub block: usize,
    /// The block's size, or the size it was freed with.
    pub size: usize,
    /// The record of the block's page.
    pub record: u32,
    /// For a block written after its free, the first word that changed,
    /// with its offset.
    pub offset: usize,
    pub found: u64,
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "heap check: {} at {:#x} (size {}, page record {:#x}",
            self.what, self.block, self.size, self.record
        )?;
        if self.what == WRITTEN {
            write!(f, ", {:#x} at offset {}", self.found, self.offset)?;
        }
        write!(f, ")")
    }
}

const WRITTEN: &str = "a freed block was written after its free";

fn page_of(block: NonNull<u8>) -> usize {
    block.as_ptr() as usize & !(PAGE_SIZE - 1)
}

/// The record of the page at `page`, or `None` when the table does not
/// cover it.
fn record(pages: &mut impl Pages, page: usize) -> Option<u32> {
    let (base, records) = pages.records();
    records.get(page.wrapping_sub(base) / PAGE_SIZE).copied()
}

fn set_record(pages: &mut impl Pages, page: usize, value: u32) {
    let (base, records) = pages.records();
    if let Some(r) = records.get_mut(page.wrapping_sub(base) / PAGE_SIZE) {
        *r = value;
    }
}

fn class_record(c: usize) -> u32 {
    c as u32 + 1
}

fn fault(what: &'static str, block: NonNull<u8>, size: usize, record: Option<u32>) -> Fault {
    Fault {
        what,
        block: block.as_ptr() as usize,
        size,
        record: record.unwrap_or(0),
        offset: 0,
        found: 0,
    }
}

/// Fills a freed block of `size` bytes at `block` after its link with the
/// mark and the poison.
///
/// # Safety
///
/// The block must be a freed block of the heap.
unsafe fn poison(block: NonNull<u8>, size: usize) {
    let words = block.cast::<u64>();
    // SAFETY: the words lie inside the block, which nothing uses.
    unsafe {
        words.add(1).write(FREED);
        for w in 2..size / 8 {
            words.add(w).write(POISON);
        }
    }
}

/// Records that whole pages, `count` of them, start at `start`.
pub fn pages_taken(pages: &mut impl Pages, start: NonNull<u8>, count: usize) {
    set_record(pages, page_of(start), LARGE | count as u32);
}

/// Checks freeing `count` whole pages at `start`, and forgets them.
pub fn pages_freed(pages: &mut impl Pages, start: NonNull<u8>, count: usize) -> Result<(), Fault> {
    let record = record(pages, page_of(start));
    if record.is_some_and(|r| r != LARGE | count as u32)
        || page_of(start) != start.as_ptr() as usize
    {
        return Err(fault(
            "freeing whole pages that are not such a block or with another size",
            start,
            count * PAGE_SIZE,
            record,
        ));
    }
    set_record(pages, page_of(start), 0);
    Ok(())
}

/// Records that the page at `page` holds blocks of class `c`, and
/// poisons them, after their links.
///
/// # Safety
///
/// The page must be the heap's, split into free blocks of the class.
pub unsafe fn refilled(pages: &mut impl Pages, page: NonNull<u8>, c: usize) {
    set_record(pages, page_of(page), class_record(c));
    let size = MIN_CLASS << c;
    for offset in (0..PAGE_SIZE).step_by(size) {
        // SAFETY: the block lies inside the page and is free.
        unsafe { poison(page.add(offset), size) };
    }
}

/// Checks the block at `block` that the free list of class `c` leads to,
/// before its link is followed: that it lies on a page of the class at
/// a block boundary and still holds its mark and poison.
///
/// # Safety
///
/// When its page is recorded with the class, the block must be readable.
pub unsafe fn taking(pages: &mut impl Pages, block: NonNull<u8>, c: usize) -> Result<(), Fault> {
    let size = MIN_CLASS << c;
    let record = record(pages, page_of(block));
    let Some(r) = record else {
        return Ok(());
    };
    if r != class_record(c) || block.as_ptr() as usize % size != 0 {
        return Err(fault(
            "a free list leads to a block that is not free",
            block,
            size,
            record,
        ));
    }
    let words = block.cast::<u64>();
    let changed = (1..size / 8).find_map(|w| {
        let expected = if w == 1 { FREED } else { POISON };
        // SAFETY: the word lies inside the block, which is on a page of
        // the class.
        let v = unsafe { words.add(w).read() };
        (v != expected).then_some((w * 8, v))
    });
    match changed {
        Some((offset, found)) => Err(Fault {
            offset,
            found,
            ..fault(WRITTEN, block, size, record)
        }),
        None => Ok(()),
    }
}

/// Clears the mark of a block taken from a free list, so that freeing it
/// is not taken for a second free.
///
/// # Safety
///
/// The block must have been checked by `taking` and now be in use.
pub unsafe fn taken(block: NonNull<u8>) {
    // SAFETY: blocks are at least 16 bytes long.
    unsafe { block.cast::<u64>().add(1).write(0) };
}

/// Checks freeing the block at `block` of class `c` with a layout of
/// `size` bytes: that it lies on a page of the class at a block boundary
/// and is not already free.
///
/// # Safety
///
/// When its page is recorded with the class, the block must be readable.
pub unsafe fn freeing(
    pages: &mut impl Pages,
    block: NonNull<u8>,
    c: usize,
    size: usize,
) -> Result<(), Fault> {
    let record = record(pages, page_of(block));
    let Some(r) = record else {
        return Ok(());
    };
    if r != class_record(c) || block.as_ptr() as usize % (MIN_CLASS << c) != 0 {
        return Err(fault(
            "freeing a block that the heap did not hand out with this size",
            block,
            size,
            record,
        ));
    }
    // SAFETY: blocks are at least 16 bytes long; the caller passes one
    // on a page of the class.
    if unsafe { block.cast::<u64>().add(1).read() } == FREED {
        return Err(fault("freeing a block twice", block, size, record));
    }
    Ok(())
}

/// Poisons a block of class `c` just put on its free list.
///
/// # Safety
///
/// The block must have been checked by `freeing` and now be free.
pub unsafe fn freed(pages: &mut impl Pages, block: NonNull<u8>, c: usize) {
    if record(pages, page_of(block)).is_some() {
        // SAFETY: the caller passes a free block of the class.
        unsafe { poison(block, MIN_CLASS << c) };
    }
}
