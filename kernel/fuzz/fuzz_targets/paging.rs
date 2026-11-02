//! Runs pager operations on the pages of one paged memory object and
//! checks them against a model of each page's state.

#![no_main]

use kernel_core::paging::{Absent, Paging};
use kernel_core::status::Error;
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::collections::{BTreeMap, BTreeSet};

const PAGES: u64 = 16;

#[derive(Arbitrary, Debug)]
enum Op {
    Access(u8),
    Supply(u8),
    Fail(u8, u8),
    Remove(u8),
    Write(u8),
    Begin(u8, u8),
    End(u8, u8, bool),
    PagerGone,
    Revoke,
}

/// The model of one page.
#[derive(Clone, Copy, Default, Debug)]
struct Page {
    present: bool,
    generation: u64,
    requested: bool,
    failed: bool,
    dirty: bool,
}

#[derive(Arbitrary, Debug)]
struct Input {
    track_dirty: bool,
    ops: Vec<Op>,
}

fuzz_target!(|input: Input| {
    let track = input.track_dirty;
    let mut p = Paging::new(track);
    let mut pages = [Page::default(); PAGES as usize];
    let mut generation = 0;
    let (mut revoked, mut gone) = (false, false);
    // The outstanding writeback: its range, recorded generations, and
    // the recorded pages written since.
    let mut writeback: Option<(u64, u64, BTreeMap<u64, u64>, BTreeSet<u64>)> = None;
    let range = |a: u8, b: u8| {
        let first = u64::from(a) % PAGES;
        (first, (first + u64::from(b) % 8).min(PAGES))
    };
    for op in input.ops.iter().take(256) {
        match *op {
            Op::Access(i) => {
                let i = u64::from(i) % PAGES;
                let m = &mut pages[i as usize];
                if m.present {
                    continue;
                }
                let expected = if revoked {
                    Absent::Revoked
                } else if m.failed || gone {
                    Absent::Failed
                } else if m.requested {
                    Absent::Wait
                } else {
                    m.requested = true;
                    Absent::Request
                };
                assert_eq!(p.absent(i), Ok(expected));
            }
            Op::Supply(i) => {
                let i = u64::from(i) % PAGES;
                let m = &mut pages[i as usize];
                let result = p.supply(i);
                if revoked {
                    assert_eq!(result, Err(Error::AccessDenied));
                } else if m.present {
                    assert_eq!(result, Ok(false));
                } else {
                    assert_eq!(result, Ok(true));
                    *m = Page { present: true, generation, ..Page::default() };
                    generation += 1;
                }
            }
            Op::Fail(a, b) => {
                let (first, end) = range(a, b);
                assert_eq!(p.fail(first, end), Ok(()));
                for m in &mut pages[first as usize..end as usize] {
                    if !m.present {
                        m.requested = false;
                        m.failed = true;
                    }
                }
            }
            Op::Remove(i) => {
                let i = u64::from(i) % PAGES;
                if !pages[i as usize].present {
                    continue;
                }
                p.removed(i);
                let recorded = writeback.as_ref().is_some_and(|w| w.2.contains_key(&i));
                let m = &mut pages[i as usize];
                m.present = false;
                m.dirty &= recorded;
            }
            Op::Write(i) => {
                let i = u64::from(i) % PAGES;
                if !pages[i as usize].present {
                    continue;
                }
                assert_eq!(p.written(i), Ok(()));
                if track {
                    pages[i as usize].dirty = true;
                    if let Some(w) = &mut writeback
                        && w.2.contains_key(&i)
                    {
                        w.3.insert(i);
                    }
                }
            }
            Op::Begin(a, b) => {
                let (first, end) = range(a, b);
                let result = p.writeback_begin(first, end);
                if !track {
                    assert_eq!(result, Err(Error::NotSupported));
                } else if writeback.is_some() {
                    assert_eq!(result, Err(Error::BadState));
                } else {
                    let recorded: BTreeMap<u64, u64> = (first..end)
                        .filter(|&i| pages[i as usize].dirty && pages[i as usize].present)
                        .map(|i| (i, pages[i as usize].generation))
                        .collect();
                    assert_eq!(result.unwrap(), recorded.keys().copied().collect::<Vec<_>>());
                    writeback = Some((first, end, recorded, BTreeSet::new()));
                }
            }
            Op::End(a, b, persisted) => {
                let (first, end) = range(a, b);
                let result = p.writeback_end(first, end, persisted);
                let matches = writeback.as_ref().is_some_and(|w| w.0 == first && w.1 == end);
                if !track {
                    assert_eq!(result, Err(Error::NotSupported));
                } else if !matches {
                    assert_eq!(result, Err(Error::BadState));
                } else {
                    assert_eq!(result, Ok(()));
                    let (_, _, recorded, written) = writeback.take().unwrap();
                    if persisted {
                        for (i, g) in recorded {
                            let m = &mut pages[i as usize];
                            if m.present && m.generation == g && !written.contains(&i) {
                                m.dirty = false;
                            }
                        }
                    }
                }
            }
            Op::PagerGone => {
                p.pager_gone();
                gone = true;
                for m in &mut pages {
                    m.requested = false;
                }
            }
            Op::Revoke => {
                p.revoke();
                revoked = true;
                for m in &mut pages {
                    m.requested = false;
                }
            }
        }
        assert_eq!(p.revoked(), revoked);
        for (i, m) in pages.iter().enumerate() {
            let i = i as u64;
            assert_eq!(p.present(i), m.present);
            let in_writeback = writeback.as_ref().is_some_and(|w| w.2.contains_key(&i));
            assert_eq!(p.evictable(i), m.present && !m.dirty && !in_writeback);
            if track {
                let pending = in_writeback && !writeback.as_ref().unwrap().3.contains(&i);
                assert_eq!(p.writable_without_fault(i), m.dirty && !pending);
            }
        }
        let dirty: Vec<u64> = p.dirty_ranges(0, PAGES).unwrap().iter().flat_map(|&(a, b)| a..b).collect();
        let expected: Vec<u64> = (0..PAGES).filter(|&i| pages[i as usize].dirty).collect();
        assert_eq!(dirty, expected);
    }
});
