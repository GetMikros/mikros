// SPDX-License-Identifier: GPL-2.0
//! Sharing the page cache of a file with a paged memory object, whose
//! stand-in on the build host records the pager ops that the shim asks
//! for.

use super::{Image, noise, start};
use crate::Mount;
use crate::host::{Disk, OBJECTS};
use crate::node::{Kind, Node};
use std::vec::Vec;

const PAGE: usize = 4096;
const ENXIO: i32 = -6;
const EINVAL: i32 = -22;
const EEXIST: i32 = -17;
const EVICT: u32 = 2;
const WRITEBACK_BEGIN: u32 = 3;
const WRITEBACK_END: u32 = 4;
const DISCARD: u32 = 6;

fn ops(cookie: u64) -> Vec<(u32, u64, u64)> {
    OBJECTS
        .lock()
        .unwrap()
        .get(&cookie)
        .map_or(Vec::new(), |o| o.ops.clone())
}

fn read(file: &Node<'_>, len: usize) -> Vec<u8> {
    let mut buf = std::vec![0; len];
    let mut done = 0;
    while done < len {
        let n = file.read(done as u64, &mut buf[done..]).unwrap();
        assert!(n > 0);
        done += n;
    }
    buf
}

/// Reclaim lets go of the folios of lent pages that the object no longer
/// has once it evicted its clean pages: those it does not call dirty. A
/// page let go of is lent anew when asked for again, and an object whose
/// pages cannot be evicted keeps them all.
#[test]
fn lets_go_of_pages_the_object_evicted() {
    const COOKIE: u64 = 43;
    start();
    let image = Image::new("unpins", 256 << 20, &[]);
    Disk::add(c"unpins", image.open(), false).unwrap();
    let data = noise(12, 4 * PAGE);
    let mount = Mount::new(c"/dev/unpins", false, None).unwrap();
    let root = mount.root().unwrap();
    let file = root.create(b"f", Kind::File, 0o644).unwrap();
    assert_eq!(file.write(0, &data), Ok(data.len()));
    let shared = file.share(COOKIE).unwrap();
    // SAFETY: the node lives until the end of the test.
    unsafe {
        for index in 0..3 {
            shared.lend_page(index).unwrap();
            shared.lent();
        }
        assert_eq!(shared.unpin(16), 0, "eviction failed");
        {
            let mut objects = OBJECTS.lock().unwrap();
            let object = objects.get_mut(&COOKIE).unwrap();
            object.evictable = true;
            object.dirty.insert(1);
        }
        assert_eq!(shared.unpin(1), 1);
        assert_eq!(shared.unpin(16), 1);
        assert_eq!(shared.unpin(16), 0, "the dirty page stays lent");
        assert!(ops(COOKIE).contains(&(EVICT, 0, 4)));
        let page = shared.lend_page(0).unwrap();
        shared.lent();
        assert!(std::slice::from_raw_parts(page, PAGE) == &data[..PAGE]);
        assert_eq!(shared.unpin(16), 1);
        shared.unshare();
    }
}

/// Reclaim that finds a lend under way has the lent pages that the object
/// no longer has let go of once it ends, while new lends wait.
#[test]
fn lets_go_of_pages_once_the_lends_under_way_end() {
    const COOKIE: u64 = 47;
    start();
    let image = Image::new("unpins-later", 256 << 20, &[]);
    Disk::add(c"unpins-later", image.open(), false).unwrap();
    let data = noise(13, 4 * PAGE);
    let mount = Mount::new(c"/dev/unpins-later", false, None).unwrap();
    let root = mount.root().unwrap();
    let file = root.create(b"f", Kind::File, 0o644).unwrap();
    assert_eq!(file.write(0, &data), Ok(data.len()));
    let shared = file.share(COOKIE).unwrap();
    OBJECTS.lock().unwrap().entry(COOKIE).or_default().evictable = true;
    let evicts = || ops(COOKIE).iter().filter(|op| op.0 == EVICT).count();
    // SAFETY: the node lives until the end of the test.
    unsafe {
        for index in 0..3 {
            shared.lend_page(index).unwrap();
            shared.lent();
        }
        shared.lend_page(3).unwrap();
        assert_eq!(shared.unpin(16), 0, "a lend is under way");
        assert_eq!(evicts(), 0);
        shared.lent();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while evicts() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "nothing let go of the pages"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // A new lend waits until the pages are let go of; of them, only
        // the page lent anew is left.
        shared.lend_page(0).unwrap();
        shared.lent();
        assert_eq!(shared.unpin(16), 1);
        shared.unshare();
    }
}

/// A lent page is the page cache's: it reads as the file, and writes
/// through the node show in it. A write through it, which the object
/// calls dirty, reaches the disk, after a writeback that evicts the
/// object's clean pages and writes the rest back as a stable writeback.
/// Truncating discards the pages cut off, and pages past the end cannot
/// be lent.
#[test]
fn shares_the_page_cache() {
    const COOKIE: u64 = 41;
    start();
    let image = Image::new("shares", 256 << 20, &[]);
    Disk::add(c"shares", image.open(), false).unwrap();
    let data = noise(11, 3 * PAGE + 100);
    {
        let mount = Mount::new(c"/dev/shares", false, None).unwrap();
        let root = mount.root().unwrap();
        assert_eq!(root.share(1).err(), Some(EINVAL));
        let file = root.create(b"f", Kind::File, 0o644).unwrap();
        assert_eq!(file.write(0, &data), Ok(data.len()));
        let shared = file.share(COOKIE).unwrap();
        assert_eq!(file.share(COOKIE).err(), Some(EEXIST));

        // SAFETY: the node lives until the end of the block.
        let (zero, one) = unsafe {
            let zero = shared.lend_page(0).unwrap();
            shared.lent();
            let one = shared.lend_page(1).unwrap();
            shared.lent();
            assert_eq!(shared.lend_page(4).err(), Some(ENXIO));
            (zero, one)
        };
        // SAFETY: the pages stay lent, and so the page cache's, until the
        // truncation below.
        let page = |at: *mut u8| unsafe { std::slice::from_raw_parts_mut(at, PAGE) };
        assert!(page(zero) == &data[..PAGE] && page(one) == &data[PAGE..2 * PAGE]);
        assert_eq!(file.write(PAGE as u64, b"node"), Ok(4));
        assert!(&page(one)[..4] == b"node");

        page(zero)[..4].copy_from_slice(b"map!");
        OBJECTS
            .lock()
            .unwrap()
            .entry(COOKIE)
            .or_default()
            .dirty
            .insert(0);
        // SAFETY: as above.
        unsafe { shared.harvest() };
        mount.sync().unwrap();
        let done = ops(COOKIE);
        for op in [EVICT, WRITEBACK_BEGIN, WRITEBACK_END] {
            assert!(done.contains(&(op, 0, 4)), "{op} in {done:?}");
        }

        file.truncate(PAGE as u64).unwrap();
        assert!(ops(COOKIE).contains(&(DISCARD, 1, 1)));
        assert!(!ops(COOKIE).contains(&(DISCARD, 0, 1)));
        // SAFETY: page 0 is not used any more.
        unsafe { shared.unshare() };
    }
    image.check();
    let mount = Mount::new(c"/dev/shares", true, None).unwrap();
    let root = mount.root().unwrap();
    let file = root.lookup(b"f").unwrap();
    assert_eq!(file.attributes().unwrap().size, PAGE as u64);
    let mut expected = data[..PAGE].to_vec();
    expected[..4].copy_from_slice(b"map!");
    assert!(read(&file, PAGE) == expected);
}
