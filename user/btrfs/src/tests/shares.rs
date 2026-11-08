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
