//! The durability run: the test init serves `tests/durable.img` with
//! `test=durability`, whose durability test driver writes six blocks on
//! the partition of each of the four scratch disks, behind virtio-blk,
//! NVMe, AHCI and USB mass storage, one after the other: a block and then
//! `Flush`, a block with `fua`, and a plain block, each followed by a
//! marker block. QEMU logs the writes and flushes that reach each disk, as
//! `test::test_args` says with `logged`. Once the test init stopped QEMU
//! as passed, each disk's log must hold the six blocks in order, a flush
//! between the first block and its marker and between the `fua` block and
//! its marker, and none between the plain block and its marker, which
//! shows that the log tells flushed blocks from others.
//!
//! QEMU's NVMe model ignores the FUA bit of its write command, so the
//! NVMe disk's `fua` block is not checked; the NVMe driver's host tests
//! check that its write command carries the bit.

use crate::build::{self, Kind};
use crate::qemu::MachineArgs;
use crate::{image, scenario, test};
use anyhow::{Result, bail};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const PASSED: &str = "SCENARIO durability PASSED";

/// What each block of the driver starts with, followed by its step as a
/// little-endian `u32`: 1 the flushed block, 2 its marker, 3 the `fua`
/// block, 4 its marker, 5 the plain block and 6 its marker. Step `n` is
/// on block `n - 1` of the partition.
const BLOCK_MAGIC: &[u8; 8] = b"MKDURABL";
const STEPS: u32 = 6;
const BLOCK: usize = 512;

/// The scratch disks of the run: the image, what the messages call it,
/// and whether its `fua` block is checked.
const DISKS: [(&str, &str, bool); 4] = [
    ("virtio.img", "virtio-blk", true),
    ("nvme.img", "NVMe", false),
    ("ahci.img", "AHCI", true),
    ("usb.img", "USB mass storage", true),
];

// QEMU's log has the format of Linux's dm-log-writes: a superblock in
// the first log sector, then each entry's header in a sector of its own,
// followed by the sectors it wrote.
const LOG_MAGIC: u64 = 0x006a_7366_7773_6872;
const LOG_VERSION: u64 = 1;
const LOG_FLUSH: u64 = 1 << 0;
const LOG_FUA: u64 = 1 << 1;
const LOG_DISCARD: u64 = 1 << 2;

/// An entry of the log: the first sector it wrote, its flags, and the
/// data it wrote.
#[derive(Debug)]
struct Entry {
    sector: u64,
    flags: u64,
    data: Vec<u8>,
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// The entries of the log `log`.
fn entries(log: &[u8]) -> Result<Vec<Entry>, String> {
    let short = || "the log ends early".to_owned();
    if u64_at(log, 0) != Some(LOG_MAGIC) || u64_at(log, 8) != Some(LOG_VERSION) {
        return Err("the log has no superblock".into());
    }
    let count = u64_at(log, 16).ok_or_else(short)?;
    let size = log.get(24..28).ok_or_else(short)?;
    let size = u32::from_le_bytes(size.try_into().unwrap()) as usize;
    if size < 32 || !size.is_power_of_two() {
        return Err(format!("the log's sector size {size} is invalid"));
    }
    let mut at = size;
    let mut entries = Vec::new();
    for _ in 0..count {
        let header = log.get(at..at + 32).ok_or_else(short)?;
        let sector = u64_at(header, 0).ok_or_else(short)?;
        let sectors = u64_at(header, 8).ok_or_else(short)?;
        let flags = u64_at(header, 16).ok_or_else(short)?;
        at += size;
        // A discard's entry holds no data.
        let len = match flags & LOG_DISCARD {
            0 => sectors as usize * size,
            _ => 0,
        };
        let data = log.get(at..at + len).ok_or_else(short)?.to_vec();
        at += len;
        entries.push(Entry {
            sector,
            flags,
            data,
        });
    }
    Ok(entries)
}

/// The index of the first entry of `entries` that writes the block of
/// step `step` where it belongs.
fn first(entries: &[Entry], step: u32) -> Option<usize> {
    let lba = test::SCRATCH_FIRST_LBA + u64::from(step) - 1;
    entries.iter().position(|e| {
        e.data.chunks(BLOCK).enumerate().any(|(i, block)| {
            e.sector + i as u64 == lba
                && block.starts_with(BLOCK_MAGIC)
                && block[8..12] == step.to_le_bytes()
        })
    })
}

/// Why the log `log` of a scratch disk does not show the driver's blocks
/// as the run expects, with its `fua` block checked when `fua`, if it
/// does not.
fn check(log: &[u8], fua: bool) -> Result<(), String> {
    let entries = entries(log)?;
    let mut at = Vec::new();
    for step in 1..=STEPS {
        let Some(i) = first(&entries, step) else {
            return Err(format!("block {step} of the driver never reached the disk"));
        };
        if at.last().is_some_and(|&last| i <= last) {
            return Err(format!(
                "block {step} reached the disk before block {}",
                step - 1
            ));
        }
        at.push(i);
    }
    let flushed = |block: usize| {
        let (from, to) = (at[block], at[block + 1]);
        entries[from].flags & LOG_FUA != 0
            || entries[from + 1..to]
                .iter()
                .any(|e| e.flags & LOG_FLUSH != 0)
    };
    if !flushed(0) {
        return Err("no flush reached the disk after the flushed block before its marker".into());
    }
    if fua && !flushed(2) {
        return Err("no flush reached the disk after the fua block before its marker".into());
    }
    if flushed(4) {
        return Err(
            "a flush reached the disk after the plain block before its marker, \
                    so the log cannot tell flushed blocks from others"
                .into(),
        );
    }
    Ok(())
}

/// Boots the durability run of the components `built` of the build kind
/// `kind` in `dir`, and checks the logs of its scratch disks.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    image::create_with(kind, built, &dir.join("disk.img"), "test=durability", &[])?;
    let log = dir.join("serial.log");
    let mut qemu = scenario::start_with(machine, dir, false, &|_| Ok(()), &[], true)?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = qemu.try_wait()? {
            break Some(status);
        }
        if Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = qemu.kill();
    let _ = qemu.wait();
    let Some(status) = status else {
        bail!(
            "the durability run timed out; serial output in {}",
            log.display()
        );
    };
    let serial = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
    if let Err(why) = test::verdict(status.code(), &serial) {
        bail!(
            "the durability run failed: {why}; serial output in {}",
            log.display()
        );
    }
    if !serial.contains(PASSED) {
        bail!(
            "the durability run failed: {PASSED:?} is missing; serial output in {}",
            log.display()
        );
    }
    for (name, what, fua) in DISKS {
        let path = dir.join(test::writes_log(name));
        if let Err(why) = check(&fs::read(&path)?, fua) {
            bail!(
                "the durability run failed on the {what} disk: {why}; its log is {}",
                path.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECTOR: usize = 512;

    /// A log of `entries`, each the first sector, the flags and the data.
    fn log(entries: &[(u64, u64, Vec<u8>)]) -> Vec<u8> {
        let mut log = vec![0; SECTOR];
        log[0..8].copy_from_slice(&LOG_MAGIC.to_le_bytes());
        log[8..16].copy_from_slice(&LOG_VERSION.to_le_bytes());
        log[16..24].copy_from_slice(&(entries.len() as u64).to_le_bytes());
        log[24..28].copy_from_slice(&(SECTOR as u32).to_le_bytes());
        for (sector, flags, data) in entries {
            let mut header = vec![0; SECTOR];
            header[0..8].copy_from_slice(&sector.to_le_bytes());
            let sectors = (data.len() / SECTOR) as u64;
            header[8..16].copy_from_slice(&sectors.to_le_bytes());
            header[16..24].copy_from_slice(&flags.to_le_bytes());
            log.extend(header);
            if flags & LOG_DISCARD == 0 {
                log.extend(data);
            }
        }
        log
    }

    fn block(step: u32) -> (u64, u64, Vec<u8>) {
        let mut data = vec![0xA5; SECTOR];
        data[..8].copy_from_slice(BLOCK_MAGIC);
        data[8..12].copy_from_slice(&step.to_le_bytes());
        (test::SCRATCH_FIRST_LBA + u64::from(step) - 1, 0, data)
    }

    fn flush() -> (u64, u64, Vec<u8>) {
        (0, LOG_FLUSH, Vec::new())
    }

    /// The log of a disk that wrote the blocks as the driver asks.
    fn durable() -> Vec<(u64, u64, Vec<u8>)> {
        vec![
            (0, 0, vec![0; 2 * SECTOR]),
            block(1),
            flush(),
            block(2),
            block(3),
            flush(),
            block(4),
            block(5),
            block(6),
            flush(),
        ]
    }

    #[test]
    fn accepts_flushes_after_the_flushed_and_fua_blocks_only() {
        assert_eq!(check(&log(&durable()), true), Ok(()));
        // A discard holds no data, and a block may be in a longer write.
        let mut entries = durable();
        entries.insert(0, (100, LOG_DISCARD, vec![0; 8 * SECTOR]));
        let (lba, _, data) = block(3);
        let mut longer = vec![0; SECTOR];
        longer.extend(data);
        entries[5] = (lba - 1, 0, longer);
        assert_eq!(check(&log(&entries), true), Ok(()));
        // A write that the device wrote with FUA needs no flush.
        let mut entries = durable();
        entries[4].1 = LOG_FUA;
        entries.remove(5);
        assert_eq!(check(&log(&entries), true), Ok(()));
    }

    #[test]
    fn refuses_a_missing_flush() {
        let mut entries = durable();
        entries.remove(2);
        assert!(
            check(&log(&entries), true)
                .unwrap_err()
                .contains("flushed block")
        );
        let mut entries = durable();
        entries.remove(5);
        assert!(
            check(&log(&entries), true)
                .unwrap_err()
                .contains("fua block")
        );
        // Unless the fua block is not checked.
        assert_eq!(check(&log(&entries), false), Ok(()));
    }

    #[test]
    fn refuses_a_flush_after_the_plain_block() {
        let mut entries = durable();
        entries.insert(8, flush());
        assert!(
            check(&log(&entries), true)
                .unwrap_err()
                .contains("plain block")
        );
    }

    #[test]
    fn refuses_missing_misplaced_and_reordered_blocks() {
        let mut entries = durable();
        entries.remove(6);
        assert!(
            check(&log(&entries), true)
                .unwrap_err()
                .contains("block 4 ")
        );
        let mut entries = durable();
        entries[3].0 += 1;
        assert!(
            check(&log(&entries), true)
                .unwrap_err()
                .contains("block 2 ")
        );
        let mut entries = durable();
        entries.swap(3, 4);
        assert!(
            check(&log(&entries), true)
                .unwrap_err()
                .contains("before block 2")
        );
    }

    #[test]
    fn refuses_a_log_without_its_superblock_or_entries() {
        assert!(check(&[0; SECTOR], true).is_err());
        let mut short = log(&durable());
        short.truncate(short.len() - 2 * SECTOR);
        assert_eq!(check(&short, true), Err("the log ends early".to_owned()));
    }
}
