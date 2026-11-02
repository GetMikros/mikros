//! Runs region operations on a small address range and compares the
//! regions with a map from pages to the region and offset they hold.

#![no_main]

use kernel_core::regions::{PAGE_SIZE, Region, Regions};
use kernel_core::status::Error;
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeMap;

/// The pages the target uses.
const PAGES: u64 = 64;

#[derive(Arbitrary, Debug)]
enum Op {
    Insert { page: u8, pages: u8, offset: u8 },
    Remove { page: u8, pages: u8 },
    Inherit { page: u8, pages: u8, value: u8 },
    Choose { pages: u8, random: u64 },
}

/// What a mapped page holds: the region's object, the object page, and
/// the inheritance.
type Page = (u32, u64, u32);

fuzz_target!(|ops: Vec<Op>| {
    let mut regions: Regions<u32> = Regions::default();
    let mut model: BTreeMap<u64, Page> = BTreeMap::new();
    for (id, op) in ops.iter().take(256).enumerate() {
        let id = id as u32;
        match *op {
            Op::Insert { page, pages, offset } => {
                let (page, pages) = (u64::from(page) % PAGES, u64::from(pages) % 8 + 1);
                let free = (page..page + pages).all(|p| !model.contains_key(&p));
                let region = Region {
                    start: page * PAGE_SIZE,
                    len: pages * PAGE_SIZE,
                    offset: u64::from(offset) * PAGE_SIZE,
                    perms: 1,
                    inheritance: 0,
                    mem: id,
                };
                assert_eq!(regions.insert(region).is_ok(), free);
                if free {
                    for p in page..page + pages {
                        model.insert(p, (id, u64::from(offset) + p - page, 0));
                    }
                }
            }
            Op::Remove { page, pages } => {
                let (start, end) = (u64::from(page) % PAGES, u64::from(page) % PAGES + u64::from(pages) % 16);
                let removed = regions.remove(start * PAGE_SIZE, end * PAGE_SIZE);
                let pages: u64 = removed.iter().map(|r| r.len / PAGE_SIZE).sum();
                let expected = (start..end).filter(|p| model.remove(p).is_some()).count() as u64;
                assert_eq!(pages, expected);
            }
            Op::Inherit { page, pages, value } => {
                let (start, end) = (u64::from(page) % PAGES, u64::from(page) % PAGES + u64::from(pages) % 16);
                let mapped = (start..end).all(|p| model.contains_key(&p));
                let value = u32::from(value % 3);
                assert_eq!(regions.set_inheritance(start * PAGE_SIZE, end * PAGE_SIZE, value).is_ok(), mapped);
                if mapped {
                    for p in start..end {
                        model.get_mut(&p).unwrap().2 = value;
                    }
                }
            }
            Op::Choose { pages, random } => {
                let pages = u64::from(pages) % 8 + 1;
                // A few values derived from the input; once they run out,
                // the choice fails like a failing random source.
                let mut values = [random, random.rotate_left(17), random.rotate_left(31)].into_iter();
                let mut exhausted = false;
                let source = || {
                    values.next().ok_or_else(|| {
                        exhausted = true;
                        Error::Internal
                    })
                };
                match regions.choose(pages * PAGE_SIZE, 0, PAGES * PAGE_SIZE, source) {
                    Ok(at) => {
                        let first = at / PAGE_SIZE;
                        assert_eq!(at % PAGE_SIZE, 0);
                        assert!(first + pages <= PAGES);
                        assert!((first..first + pages).all(|p| !model.contains_key(&p)));
                    }
                    Err(_) if exhausted => {}
                    Err(_) => {
                        let fits = (0..=PAGES - pages).any(|f| (f..f + pages).all(|p| !model.contains_key(&p)));
                        assert!(!fits);
                    }
                }
            }
        }
        // Every mapped page is found in a region that maps the same
        // object page, and every other page is free.
        let mut previous_end = 0;
        for r in regions.iter() {
            assert!(r.start >= previous_end && r.len > 0);
            previous_end = r.end();
        }
        for p in 0..PAGES + 16 {
            let found = regions.find(p * PAGE_SIZE).map(|r| (r.mem, r.page(p * PAGE_SIZE), r.inheritance));
            assert_eq!(found, model.get(&p).copied());
        }
    }
});
