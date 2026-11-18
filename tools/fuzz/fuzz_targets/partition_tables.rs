//! The partition driver's parsers of what a disk holds
//! (MIKSPEC-0017.R26): a GPT header and its entry array, a master boot
//! record, and the file system probes, fed the bytes of a hostile disk.
//! The first 512 bytes are a block; the rest is an entry array, and the
//! whole input the start of a partition for the probes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use user_fuzz::gpt::{header, mbr, probe};

fuzz_target!(|data: &[u8]| {
    let (block, rest) = data.split_at(data.len().min(512));
    if let Some(found) = header(block, 1) {
        let _ = found.partitions(rest);
    }
    if mbr::is_mbr(block) {
        let _ = mbr::signature(block);
        let _ = mbr::primaries(block);
    }
    let _ = probe::find(data, data.len() as u64);
});
