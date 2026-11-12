//! Allocates and frees ranges of a small space, whose free ranges are
//! more than fit in the ranges themselves, at times without memory for
//! more room, and compares every result and the number of free ranges
//! with a model that keeps one bit per address.

#![no_main]

use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, Ordering};
use kernel_core::ranges::{Error, INLINE, Ranges, Room};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::alloc::Layout;

/// The largest space, in addresses.
const SPACE: u64 = 512;

/// Whether room for more free ranges runs out.
static NO_MEMORY: AtomicBool = AtomicBool::new(false);

/// Room on the heap, which runs out while `NO_MEMORY` says so.
struct HeapRoom;

impl Room for HeapRoom {
    type Handle = NonNull<(u64, u64)>;

    fn take(count: usize) -> Option<(NonNull<(u64, u64)>, Self::Handle)> {
        if NO_MEMORY.load(Ordering::Relaxed) {
            return None;
        }
        let layout = Layout::array::<(u64, u64)>(count).ok()?;
        // SAFETY: the layout is not empty.
        let room = NonNull::new(unsafe { std::alloc::alloc(layout) }.cast())?;
        Some((room, room))
    }

    unsafe fn give(room: Self::Handle, count: usize) {
        let layout = Layout::array::<(u64, u64)>(count).unwrap();
        // SAFETY: the room came from the heap with this layout.
        unsafe { std::alloc::dealloc(room.as_ptr().cast(), layout) };
    }
}

#[derive(Arbitrary, Debug)]
enum Op {
    Alloc { size: u16, align_log: u8 },
    /// Frees `size` addresses at `offset` from the start of the space.
    Free { offset: u16, size: u16 },
    FreeRaw { start: u64, size: u64 },
    /// Makes room for more free ranges run out, or not.
    NoMemory(bool),
}

#[derive(Arbitrary, Debug)]
struct Input {
    /// The start of the space; near the top it tests overflow.
    start: u64,
    len: u16,
    ops: Vec<Op>,
}

struct Model {
    start: u64,
    /// Whether each address of the space is free.
    free: Vec<bool>,
    /// How many free ranges there is room for, which doubles as needed.
    room: usize,
}

impl Model {
    /// The free ranges, lowest first, as start and end.
    fn runs(&self) -> Vec<(u64, u64)> {
        let mut out: Vec<(u64, u64)> = Vec::new();
        for (i, &f) in self.free.iter().enumerate() {
            let a = self.start + i as u64;
            match out.last_mut() {
                Some(last) if f && last.1 == a => last.1 = a + 1,
                _ if f => out.push((a, a + 1)),
                _ => {}
            }
        }
        out
    }

    fn set(&mut self, start: u64, size: u64, free: bool) {
        let at = (start - self.start) as usize;
        self.free[at..at + size as usize].fill(free);
    }

    /// The lowest aligned start in a free range that has room, skipping
    /// ranges it would have to split when there is no room for another
    /// and no memory for more.
    fn alloc(&mut self, size: u64, align: u64) -> Option<u64> {
        if size == 0 || !align.is_power_of_two() {
            return None;
        }
        let runs = self.runs();
        for &(start, end) in &runs {
            let Some(at) = start.checked_next_multiple_of(align) else {
                continue;
            };
            if at.checked_add(size).is_none_or(|e| e > end) {
                continue;
            }
            if at > start && at + size < end && runs.len() == self.room {
                if NO_MEMORY.load(Ordering::Relaxed) {
                    continue;
                }
                self.room *= 2;
            }
            self.set(at, size, false);
            return Some(at);
        }
        None
    }

    fn free(&mut self, start: u64, size: u64) -> Result<(), Error> {
        let bad = Err(Error::NotAllocated { start, size });
        let end = self.start + self.free.len() as u64;
        let Some(stop) = start.checked_add(size) else {
            return bad;
        };
        if size == 0 || start < self.start || stop > end {
            return bad;
        }
        let at = (start - self.start) as usize;
        if self.free[at..at + size as usize].iter().any(|&f| f) {
            return bad;
        }
        let before = self.runs().len();
        self.set(start, size, true);
        if self.runs().len() > self.room {
            assert_eq!(before, self.room);
            if NO_MEMORY.load(Ordering::Relaxed) {
                self.set(start, size, false);
                return Err(Error::NoMemory);
            }
            self.room *= 2;
        }
        Ok(())
    }
}

fuzz_target!(|input: Input| {
    let len = u64::from(input.len) % (SPACE + 1);
    let start = input.start.min(u64::MAX - len);
    NO_MEMORY.store(false, Ordering::Relaxed);
    let mut ranges = Ranges::<HeapRoom>::new(start, start + len);
    let mut model = Model {
        start,
        free: vec![true; len as usize],
        room: INLINE,
    };
    for op in input.ops.iter().take(256) {
        match *op {
            Op::Alloc { size, align_log } => {
                let size = u64::from(size) % (SPACE + 2);
                // Some values are not powers of two.
                let align = match align_log {
                    l @ 0..64 => 1u64 << l,
                    l => u64::from(l),
                };
                assert_eq!(ranges.alloc(size, align), model.alloc(size, align));
            }
            Op::Free { offset, size } => {
                let at = start.saturating_add(u64::from(offset) % (SPACE + 2));
                let size = u64::from(size) % (SPACE + 2);
                assert_eq!(ranges.free(at, size), model.free(at, size));
            }
            Op::FreeRaw { start, size } => {
                assert_eq!(ranges.free(start, size), model.free(start, size));
            }
            Op::NoMemory(none) => NO_MEMORY.store(none, Ordering::Relaxed),
        }
        assert_eq!(ranges.count(), model.runs().len());
    }
});
