//! Finding the system partition in the boot disk's GUID partition table.

use crate::cmdline::guid_text;
use crate::crc::crc32;
use crate::disk::{Disk, IoError};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

/// The type GUID of a Mikros root partition, as GPT stores it.
pub const ROOT_TYPE: [u8; 16] = [
    0xf6, 0x0a, 0x73, 0xfa, 0xa3, 0x8b, 0xd9, 0x42, 0xb1, 0x55, 0x46, 0x69, 0x09, 0xd0, 0x81, 0x35,
];

const SIGNATURE: &[u8; 8] = b"EFI PART";
const HEADER_MIN: u32 = 92;
const ENTRY_MIN: u32 = 128;
/// The largest partition entry array that is read.
const ENTRIES_MAX: u64 = 1 << 20;

/// A partition: its type GUID and partition GUID as stored, and its
/// first and last LBA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Partition {
    pub type_guid: [u8; 16],
    pub guid: [u8; 16],
    pub first_lba: u64,
    pub last_lba: u64,
}

impl Partition {
    /// The partition's start and size in bytes, when its blocks lie on a
    /// disk whose last block is `last_lba`.
    pub fn range(&self, block_size: u32, last_lba: u64) -> Option<(u64, u64)> {
        if self.first_lba > self.last_lba || self.last_lba > last_lba {
            return None;
        }
        let block = u64::from(block_size);
        let start = self.first_lba.checked_mul(block)?;
        let size = (self.last_lba - self.first_lba + 1).checked_mul(block)?;
        Some((start, size))
    }
}

/// Why no system partition was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Io(IoError),
    /// Neither the primary nor the backup table is valid.
    NoTable,
    /// No partition matches, or more than one does; with the partition
    /// GUIDs of the Mikros root partitions.
    NotOne {
        wanted: Option<[u8; 16]>,
        candidates: Vec<[u8; 16]>,
    },
    /// The command line gives no valid partition GUID; with the partition
    /// GUIDs of the Mikros root partitions.
    BadGuid {
        candidates: Vec<[u8; 16]>,
    },
}

impl From<IoError> for Error {
    fn from(e: IoError) -> Error {
        Error::Io(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let candidates = match self {
            Error::Io(e) => return write!(f, "{e}"),
            Error::NoTable => {
                return write!(f, "the boot disk has no valid GUID partition table");
            }
            Error::NotOne {
                wanted: Some(guid),
                candidates,
            } => {
                write!(f, "no partition {} on the boot disk", guid_text(guid))?;
                candidates
            }
            Error::NotOne {
                wanted: None,
                candidates,
            } if candidates.is_empty() => {
                write!(f, "the boot disk has no Mikros root partition")?;
                candidates
            }
            Error::NotOne { candidates, .. } => {
                write!(f, "the boot disk has more than one Mikros root partition")?;
                candidates
            }
            Error::BadGuid { candidates } => {
                write!(f, "the root=PARTUUID= option holds no valid GUID")?;
                candidates
            }
        };
        write!(f, "; Mikros root partitions:")?;
        if candidates.is_empty() {
            write!(f, " none")?;
        }
        for guid in candidates {
            write!(f, " {}", guid_text(guid))?;
        }
        Ok(())
    }
}

/// The partition GUIDs of the Mikros root partitions among `partitions`.
pub fn roots(partitions: &[Partition]) -> Vec<[u8; 16]> {
    partitions
        .iter()
        .filter(|p| p.type_guid == ROOT_TYPE)
        .map(|p| p.guid)
        .collect()
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// The partitions of the table whose header is at `lba`, when the header
/// and its entry array are valid.
fn table(
    disk: &mut impl Disk,
    block_size: u32,
    last_lba: u64,
    lba: u64,
) -> Result<Option<Vec<Partition>>, IoError> {
    let block = u64::from(block_size);
    let mut header = vec![0; block_size as usize];
    disk.read(lba * block, &mut header)?;
    let size = u32_at(&header, 12);
    if &header[..8] != SIGNATURE || size < HEADER_MIN || size > block_size {
        return Ok(None);
    }
    let mut copy = header[..size as usize].to_vec();
    copy[16..20].fill(0);
    let entries_lba = u64_at(&header, 72);
    let count = u64::from(u32_at(&header, 80));
    let entry_size = u32_at(&header, 84);
    let entries_size = count * u64::from(entry_size);
    let valid = crc32(&copy) == u32_at(&header, 16)
        && u64_at(&header, 24) == lba
        && entry_size >= ENTRY_MIN
        && entry_size % 8 == 0
        && entries_size <= ENTRIES_MAX
        && entries_lba != lba
        && entries_lba <= last_lba
        && entries_size.div_ceil(block) <= last_lba - entries_lba + 1;
    if !valid {
        return Ok(None);
    }
    let mut entries = vec![0; entries_size as usize];
    disk.read(entries_lba * block, &mut entries)?;
    if crc32(&entries) != u32_at(&header, 88) {
        return Ok(None);
    }
    let partitions = entries
        .chunks_exact(entry_size as usize)
        .filter(|e| e[..16] != [0; 16])
        .map(|e| Partition {
            type_guid: e[..16].try_into().expect("16 bytes"),
            guid: e[16..32].try_into().expect("16 bytes"),
            first_lba: u64_at(e, 32),
            last_lba: u64_at(e, 40),
        })
        .collect();
    Ok(Some(partitions))
}

/// The partitions of the disk's primary table at LBA 1, or of the backup
/// table at `last_lba` when the primary one is not valid.
pub fn partitions(
    disk: &mut impl Disk,
    block_size: u32,
    last_lba: u64,
) -> Result<Vec<Partition>, Error> {
    if block_size < 512 || last_lba < 2 {
        return Err(Error::NoTable);
    }
    if let Some(found) = table(disk, block_size, last_lba, 1)? {
        return Ok(found);
    }
    table(disk, block_size, last_lba, last_lba)?.ok_or(Error::NoTable)
}

/// The system partition among `partitions`: the one whose partition GUID
/// is `wanted` or, without it, the only Mikros root partition.
pub fn system_partition(
    partitions: &[Partition],
    wanted: Option<[u8; 16]>,
) -> Result<Partition, Error> {
    let found: Vec<&Partition> = match wanted {
        Some(guid) => partitions.iter().filter(|p| p.guid == guid).collect(),
        None => partitions
            .iter()
            .filter(|p| p.type_guid == ROOT_TYPE)
            .collect(),
    };
    match found[..] {
        [one] => Ok(*one),
        _ => Err(Error::NotOne {
            wanted,
            candidates: roots(partitions),
        }),
    }
}

#[cfg(test)]
mod tests;
