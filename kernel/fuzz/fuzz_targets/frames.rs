//! Runs frame allocator operations and compares the allocator with a set
//! of free frames.

#![no_main]

use kernel_core::frames::{Frames, MAX_ALIGN};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeSet;

/// The frames the bitmap covers.
const FRAMES: u64 = 2048;
const PAGE: u64 = 4096;

#[derive(Arbitrary, Debug)]
enum Op {
    Add { first: u16, count: u16 },
    Reserve { first: u16, count: u16 },
    Alloc,
    AllocContiguous { count: u16, align_log: u8 },
    /// Frees an earlier allocation, picked by index.
    Free(u8),
}

fuzz_target!(|ops: Vec<Op>| {
    let mut bits = vec![0u64; (FRAMES / 64) as usize];
    let mut frames = Frames::new(&mut bits);
    let mut free: BTreeSet<u64> = BTreeSet::new();
    let mut allocations: Vec<(u64, u64)> = Vec::new();
    for op in ops.iter().take(512) {
        match *op {
            Op::Add { first, count } => {
                // Only frames that are not allocated may be added.
                let (first, count) = (u64::from(first) % (FRAMES + 64), u64::from(count) % 256);
                let overlap = allocations.iter().any(|&(f, n)| f < first + count && first < f + n);
                if !overlap {
                    frames.add(first, count);
                    free.extend((first..first + count).filter(|&f| f < FRAMES));
                }
            }
            Op::Reserve { first, count } => {
                let (first, count) = (u64::from(first) % FRAMES, u64::from(count) % 256);
                frames.reserve(first, count);
                for f in first..first + count {
                    free.remove(&f);
                }
            }
            Op::Alloc => match frames.alloc() {
                Some(phys) => {
                    assert_eq!(phys % PAGE, 0);
                    assert!(free.remove(&(phys / PAGE)));
                    allocations.push((phys / PAGE, 1));
                }
                None => assert!(free.is_empty()),
            },
            Op::AllocContiguous { count, align_log } => {
                let count = u64::from(count) % 64 + 1;
                let align = 1u64 << (u32::from(align_log) % (MAX_ALIGN.trailing_zeros() + 1));
                match frames.alloc_contiguous(count, align) {
                    Some(phys) => {
                        let first = phys / PAGE;
                        assert_eq!(first % align, 0);
                        for f in first..first + count {
                            assert!(free.remove(&f));
                        }
                        allocations.push((first, count));
                    }
                    None => {
                        let fits = (0..FRAMES).step_by(align as usize).any(|s| (s..s + count).all(|f| free.contains(&f)));
                        assert!(!fits);
                    }
                }
            }
            Op::Free(i) => {
                if allocations.is_empty() {
                    continue;
                }
                let (first, count) = allocations.swap_remove(usize::from(i) % allocations.len());
                frames.free(first * PAGE, count);
                free.extend(first..first + count);
            }
        }
        assert_eq!(frames.free_count(), free.len() as u64);
    }
});
