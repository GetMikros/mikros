//! The file allocation table: its entries, cluster chains and the
//! clean-shutdown bit.

use crate::bpb::{FatType, Layout};
use crate::disk::Disk;
use crate::{Device, Error};

/// What a FAT entry says of its cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Free,
    Next(u32),
    End,
    Bad,
}

/// The clean-shutdown bit of FAT entry 1, for FAT16 and FAT32.
fn clean_bit(fat_type: FatType) -> Option<u32> {
    match fat_type {
        FatType::Fat12 => None,
        FatType::Fat16 => Some(0x8000),
        FatType::Fat32 => Some(0x0800_0000),
    }
}

/// Where FAT entry `n` lies in the FAT, in bytes, and how many bytes it
/// takes.
fn place(layout: &Layout, n: u32) -> (u64, usize) {
    let n = u64::from(n);
    match layout.fat_type {
        FatType::Fat12 => (n + n / 2, 2),
        FatType::Fat16 => (n * 2, 2),
        FatType::Fat32 => (n * 4, 4),
    }
}

/// The FATs that are read: the active one, or the first.
fn read_fat(layout: &Layout) -> u64 {
    u64::from(layout.active_fat.unwrap_or(0))
}

/// The raw value of FAT entry `n`.
pub(crate) fn raw<D: Device>(disk: &mut Disk<D>, layout: &Layout, n: u32) -> Result<u32, Error> {
    let (at, len) = place(layout, n);
    let mut bytes = [0u8; 4];
    let start = layout.fat_start + read_fat(layout) * layout.fat_size;
    disk.read(start + at, &mut bytes[..len])?;
    let value = u32::from_le_bytes(bytes);
    Ok(match layout.fat_type {
        FatType::Fat12 if n % 2 == 1 => value >> 4,
        FatType::Fat12 => value & 0xFFF,
        FatType::Fat16 => value,
        FatType::Fat32 => value & 0x0FFF_FFFF,
    })
}

/// Writes the raw value of FAT entry `n` to every FAT that is kept, or
/// to the active one when mirroring is off.
pub(crate) fn set_raw<D: Device>(
    disk: &mut Disk<D>,
    layout: &Layout,
    n: u32,
    value: u32,
) -> Result<(), Error> {
    let (at, len) = place(layout, n);
    let fats: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7];
    let fats = match layout.active_fat {
        Some(active) => &fats[usize::from(active)..=usize::from(active)],
        None => &fats[..usize::from(layout.fats.min(8))],
    };
    for &fat in fats {
        let where_ = layout.fat_start + u64::from(fat) * layout.fat_size + at;
        let mut bytes = [0u8; 4];
        disk.read(where_, &mut bytes[..len])?;
        let old = u32::from_le_bytes(bytes);
        let new = match layout.fat_type {
            FatType::Fat12 if n % 2 == 1 => old & 0x000F | (value & 0xFFF) << 4,
            FatType::Fat12 => old & 0xF000 | value & 0xFFF,
            FatType::Fat16 => value & 0xFFFF,
            FatType::Fat32 => old & 0xF000_0000 | value & 0x0FFF_FFFF,
        };
        disk.write(where_, &new.to_le_bytes()[..len])?;
    }
    Ok(())
}

/// The bytes of the FAT that counting free clusters reads at once.
const COUNT_CHUNK: usize = 64 << 10;

/// How many clusters the FAT that is read says are free. FAT16 and FAT32
/// tables are read in pieces of `COUNT_CHUNK` bytes rather than entry
/// by entry, which takes one request of the device for each sector;
/// FAT12 tables take a few sectors at most.
pub(crate) fn count_free<D: Device>(disk: &mut Disk<D>, layout: &Layout) -> Result<u32, Error> {
    let width = match layout.fat_type {
        FatType::Fat12 => {
            let mut free = 0;
            for cluster in 2..layout.clusters + 2 {
                free += u32::from(get(disk, layout, cluster)? == Entry::Free);
            }
            return Ok(free);
        }
        FatType::Fat16 => 2,
        FatType::Fat32 => 4,
    };
    let start = layout.fat_start + read_fat(layout) * layout.fat_size;
    // Entries 2 to clusters + 1 count, in whole sectors from the first.
    let entries = u64::from(layout.clusters) + 2;
    let end = (entries * width).next_multiple_of(disk.sector());
    let mut buf = alloc::vec![0u8; COUNT_CHUNK];
    let (mut at, mut free) = (0, 0);
    while at < end {
        let len = (end - at).min(COUNT_CHUNK as u64) as usize;
        disk.read_direct(start + at, &mut buf[..len])?;
        let first = at / width;
        for (i, entry) in buf[..len].chunks_exact(width as usize).enumerate() {
            let n = first + i as u64;
            if n < 2 || n >= entries {
                continue;
            }
            let value = match entry {
                [a, b] => u32::from(u16::from_le_bytes([*a, *b])),
                [a, b, c, d] => u32::from_le_bytes([*a, *b, *c, *d]) & 0x0FFF_FFFF,
                _ => unreachable!("entries of 2 or 4 bytes"),
            };
            free += u32::from(value == 0);
        }
        at += len as u64;
    }
    Ok(free)
}

/// What FAT entry `cluster` says.
pub(crate) fn get<D: Device>(
    disk: &mut Disk<D>,
    layout: &Layout,
    cluster: u32,
) -> Result<Entry, Error> {
    let value = raw(disk, layout, cluster)?;
    let (bad, end) = match layout.fat_type {
        FatType::Fat12 => (0xFF7, 0xFF8),
        FatType::Fat16 => (0xFFF7, 0xFFF8),
        FatType::Fat32 => (0x0FFF_FFF7, 0x0FFF_FFF8),
    };
    Ok(match value {
        0 => Entry::Free,
        v if v >= end => Entry::End,
        v if v == bad => Entry::Bad,
        v if layout.is_data(v) => Entry::Next(v),
        _ => return Err(Error::Io),
    })
}

/// Makes FAT entry `cluster` say `entry`.
pub(crate) fn set<D: Device>(
    disk: &mut Disk<D>,
    layout: &Layout,
    cluster: u32,
    entry: Entry,
) -> Result<(), Error> {
    let value = match entry {
        Entry::Free => 0,
        Entry::Next(next) => next,
        Entry::End => 0x0FFF_FFFF,
        Entry::Bad => match layout.fat_type {
            FatType::Fat12 => 0xFF7,
            FatType::Fat16 => 0xFFF7,
            FatType::Fat32 => 0x0FFF_FFF7,
        },
    };
    set_raw(disk, layout, cluster, value)
}

/// The cluster after `cluster` in its chain, or None at its end. A free
/// or bad cluster in a chain, or one outside the volume, fails with
/// `Io`.
pub(crate) fn next<D: Device>(
    disk: &mut Disk<D>,
    layout: &Layout,
    cluster: u32,
) -> Result<Option<u32>, Error> {
    match get(disk, layout, cluster)? {
        Entry::Next(next) => Ok(Some(next)),
        Entry::End => Ok(None),
        Entry::Free | Entry::Bad => Err(Error::Io),
    }
}

/// Whether the volume was shut down cleanly, from the clean-shutdown bit
/// of FAT entry 1. FAT12 has none and is always clean.
pub(crate) fn clean<D: Device>(disk: &mut Disk<D>, layout: &Layout) -> Result<bool, Error> {
    match clean_bit(layout.fat_type) {
        None => Ok(true),
        Some(bit) => Ok(raw(disk, layout, 1)? & bit != 0),
    }
}

/// Sets or clears the clean-shutdown bit of FAT entry 1.
pub(crate) fn set_clean<D: Device>(
    disk: &mut Disk<D>,
    layout: &Layout,
    clean: bool,
) -> Result<(), Error> {
    let Some(bit) = clean_bit(layout.fat_type) else {
        return Ok(());
    };
    let value = raw(disk, layout, 1)?;
    let value = if clean { value | bit } else { value & !bit };
    set_raw(disk, layout, 1, value)
}
