//! The file allocation table: its entries and cluster chains.

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
