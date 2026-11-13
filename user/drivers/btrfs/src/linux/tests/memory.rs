// SPDX-License-Identifier: GPL-2.0
//! The shim's memory, which reclaim keeps within what the host gave.

use super::{Image, noise, start};
use crate::linux::host::{Disk, MEMORY};
use crate::linux::node::Kind;
use crate::linux::{Mount, memory_kept, memory_used};

/// Puts the number of MiB `i` of a file in each 4 KiB block of `buf`.
fn tag(buf: &mut [u8], i: u64) {
    for block in buf.chunks_mut(4096) {
        block[..8].copy_from_slice(&i.to_le_bytes());
    }
}

/// Asserts that the shim holds no more than its memory, give or take
/// what the other tests that run meanwhile may hold above it.
fn assert_within_memory(when: &str) {
    let used = memory_used();
    let limit = MEMORY as u64 + MEMORY as u64 / 8;
    assert!(
        used <= limit,
        "{when}: the shim holds {used} bytes, more than {limit}"
    );
}

/// Writing and reading back a file twice as large as the shim's memory
/// leaves the shim within it: reclaim drops pages of the page cache that
/// are written back, and those that were read.
#[test]
fn reclaims_the_page_cache() {
    let mib = (2 * MEMORY as u64) >> 20;
    start();
    let image = Image::new("reclaims", 4 << 30, &[]);
    Disk::add(c"reclaims", image.open(), false).unwrap();
    let mount = Mount::new(c"/dev/reclaims", false, None).unwrap();
    {
        let root = mount.root().unwrap();
        let file = root.create(b"big", Kind::File, 0o644).unwrap();
        let mut buf = noise(9, 1 << 20);
        for i in 0..mib {
            tag(&mut buf, i);
            assert_eq!(file.write(i << 20, &buf), Ok(1 << 20));
        }
        file.sync().unwrap();
        assert_within_memory("after writing");
        let mut expected = noise(9, 1 << 20);
        for i in 0..mib {
            tag(&mut expected, i);
            assert_eq!(file.read(i << 20, &mut buf), Ok(1 << 20));
            assert!(buf == expected, "MiB {i}");
        }
        assert_within_memory("after reading");
    }
    drop(mount);
    image.check();
}

/// Once the pages of a file of half the shim's memory are dropped, the
/// shim keeps 8 free blocks of 4 MiB and gives the rest back to the host;
/// up to 8 more may be on their way back for other tests meanwhile.
#[test]
fn gives_free_memory_back() {
    start();
    let image = Image::new("gives-back", 1 << 30, &[]);
    Disk::add(c"gives-back", image.open(), false).unwrap();
    let mount = Mount::new(c"/dev/gives-back", false, None).unwrap();
    {
        let root = mount.root().unwrap();
        let file = root.create(b"big", Kind::File, 0o644).unwrap();
        let buf = noise(5, 1 << 20);
        for i in 0..(MEMORY as u64 / 2) >> 20 {
            assert_eq!(file.write(i << 20, &buf), Ok(1 << 20));
        }
        file.sync().unwrap();
    }
    drop(mount);
    let kept = memory_kept();
    assert!(kept <= 64 << 20, "the shim keeps {kept} free bytes");
    image.check();
}
