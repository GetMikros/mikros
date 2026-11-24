//! The pages of private file mappings that still map the file's object
//! itself. A private mapping that cannot be written maps that object, and
//! `mprotect` adding the right to write replaces the pages that still do
//! with a clone of exactly those pages, so these pages are remembered with
//! the handle of the object.

use crate::lock::Lock;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::OwnedHandle;

/// A run of pages of the address space that map the object `source` from
/// `offset`.
#[derive(Clone)]
pub struct Run {
    pub start: u64,
    pub len: u64,
    pub source: Arc<OwnedHandle>,
    pub offset: u64,
}

/// The runs, by their first address. They never overlap.
static RUNS: Lock<BTreeMap<u64, Run>> = Lock::new(BTreeMap::new());

/// Remembers that the `len` bytes from `start` map `source` from `offset`.
pub fn track(start: u64, len: u64, source: Arc<OwnedHandle>, offset: u64) {
    let mut runs = RUNS.lock();
    forget_in(&mut runs, start, len);
    runs.insert(
        start,
        Run {
            start,
            len,
            source,
            offset,
        },
    );
}

/// Forgets the pages of the `len` bytes from `start`, which another mapping
/// replaces or which are unmapped.
pub fn forget(start: u64, len: u64) {
    forget_in(&mut RUNS.lock(), start, len);
}

/// The parts of the remembered runs inside the `len` bytes from `start`.
pub fn within(start: u64, len: u64) -> Vec<Run> {
    overlapping(&RUNS.lock(), start, len)
}

/// The parts of the runs that overlap `start..start + len`, cut to it.
fn overlapping(runs: &BTreeMap<u64, Run>, start: u64, len: u64) -> Vec<Run> {
    let end = start.saturating_add(len);
    runs.values()
        .filter(|run| run.start < end && run.start + run.len > start)
        .map(|run| {
            let from = run.start.max(start);
            let to = (run.start + run.len).min(end);
            Run {
                start: from,
                len: to - from,
                source: run.source.clone(),
                offset: run.offset + (from - run.start),
            }
        })
        .collect()
}

fn forget_in(runs: &mut BTreeMap<u64, Run>, start: u64, len: u64) {
    let end = start.saturating_add(len);
    let hit: Vec<Run> = runs
        .values()
        .filter(|run| run.start < end && run.start + run.len > start)
        .cloned()
        .collect();
    for run in hit {
        runs.remove(&run.start);
        // What lies before and after the forgotten range stays.
        if run.start < start {
            runs.insert(
                run.start,
                Run {
                    len: start - run.start,
                    ..run.clone()
                },
            );
        }
        if run.start + run.len > end {
            runs.insert(
                end,
                Run {
                    start: end,
                    len: run.start + run.len - end,
                    source: run.source.clone(),
                    offset: run.offset + (end - run.start),
                },
            );
        }
    }
}
