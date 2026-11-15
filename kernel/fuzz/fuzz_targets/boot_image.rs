//! Checks a boot module array, read from the input as bootloader memory
//! next to boot image memory, and lays out the boot image object of an
//! array that passes: its index and the place of each module's contents.

#![no_main]

use boot_protocol::{Kind, MemoryMapEntry, PAGE_SIZE};
use kernel_core::modules::{self, Module};
use libfuzzer_sys::fuzz_target;

/// Where the input lies, as bootloader memory, and the boot image memory
/// after it.
const INPUT: u64 = 0x10_0000;
const BOOT_IMAGE: u64 = 0x1000_0000;
const BOOT_IMAGE_PAGES: u64 = 256;

fuzz_target!(|data: &[u8]| {
    let mut memory = data.to_vec();
    memory.resize(data.len().next_multiple_of(PAGE_SIZE as usize), 0);
    let map = [
        MemoryMapEntry {
            phys_start: INPUT,
            page_count: memory.len() as u64 / PAGE_SIZE,
            kind: Kind::BootloaderReclaimable as u32,
            reserved: 0,
        },
        MemoryMapEntry {
            phys_start: BOOT_IMAGE,
            page_count: BOOT_IMAGE_PAGES,
            kind: Kind::BootImage as u32,
            reserved: 0,
        },
    ];
    let entries = modules::decode(data);
    if modules::check_array(INPUT, entries.len() as u64, map.into_iter()).is_err() {
        return;
    }
    // Only paths that lie in bootloader memory, the input, are read.
    let path = |m: &Module| {
        let at = (m.path_phys - INPUT) as usize;
        &memory[at..at + m.path_len as usize]
    };
    if modules::check(&entries, map.into_iter(), path).is_err() {
        return;
    }
    let sizes: Vec<u64> = entries.iter().map(|m| m.size).collect();
    let paths: Vec<&[u8]> = entries.iter().map(path).collect();
    let layout = modules::layout(&sizes, &paths);
    let index = layout.index.len() as u64;
    assert_eq!(index % PAGE_SIZE, 0);
    assert_eq!(&layout.index[..8], b"MIKROSBM");
    let mut placed: Vec<(u64, u64)> = Vec::new();
    for (i, (&size, &offset)) in sizes.iter().zip(&layout.offsets).enumerate() {
        let entry = 16 + 24 * i;
        let field = |at: usize| {
            u64::from_le_bytes(layout.index[entry + at..entry + at + 8].try_into().unwrap())
        };
        assert_eq!((field(0), field(8)), (offset, size));
        let at = u32::from_le_bytes(layout.index[entry + 16..entry + 20].try_into().unwrap());
        assert_eq!(&layout.index[at as usize..][..paths[i].len()], paths[i]);
        if size == 0 {
            assert_eq!(offset, 0);
            continue;
        }
        // Contents start on a page of their own, after the index, and
        // stay inside the object.
        assert_eq!(offset % PAGE_SIZE, 0);
        assert!(offset >= index);
        let end = offset + size.next_multiple_of(PAGE_SIZE);
        assert!(end <= layout.size);
        assert!(placed.iter().all(|&(s, e)| end <= s || offset >= e));
        placed.push((offset, end));
    }
});
