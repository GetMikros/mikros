//! Reading GUID partition tables, for the partition driver: checking a
//! header and its partition entry array, and listing the partitions.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

#[cfg(test)]
mod tests;

/// The signature that a GPT header starts with.
const SIGNATURE: &[u8; 8] = b"EFI PART";
/// The size of the header fields this reader knows, and of a partition
/// entry at the least.
const HEADER_MIN: u32 = 92;
const ENTRY_MIN: u32 = 128;
/// Where a partition entry holds its name, and how many UTF-16 code
/// units the name has.
const NAME_AT: usize = 56;
const NAME_UNITS: usize = 36;

/// A GPT header: the LBA it lies at and that of the other one, the
/// usable LBAs, and where its partition entry array is, how many entries
/// it has of which size, and the array's CRC32.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub my_lba: u64,
    pub alternate_lba: u64,
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_size: u32,
    pub entries_crc32: u32,
}

/// A partition of a table: its 1-based index in the entry array, its
/// type GUID and partition GUID as stored, its first and last LBA, its
/// attributes and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    pub index: u32,
    pub type_guid: [u8; 16],
    pub guid: [u8; 16],
    pub first_lba: u64,
    pub last_lba: u64,
    pub attributes: u64,
    pub name: String,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// The CRC32 of `bytes`, as GPT computes it: reflected, with polynomial
/// 0x04C11DB7, starting at all ones and inverted at the end.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// The header in `block`, read from LBA `lba`, when it is valid: it has
/// the signature, a header size from 92 bytes to the block's size, a
/// CRC32 that matches, the LBA it was read from, entries of at least
/// 128 bytes in a multiple of 8, and an entry array beyond itself.
pub fn header(block: &[u8], lba: u64) -> Option<Header> {
    if block.len() < HEADER_MIN as usize || &block[..8] != SIGNATURE {
        return None;
    }
    let size = u32_at(block, 12);
    if size < HEADER_MIN || size as usize > block.len() {
        return None;
    }
    let mut copy = block[..size as usize].to_vec();
    copy[16..20].fill(0);
    if crc32(&copy) != u32_at(block, 16) {
        return None;
    }
    let header = Header {
        my_lba: u64_at(block, 24),
        alternate_lba: u64_at(block, 32),
        first_usable_lba: u64_at(block, 40),
        last_usable_lba: u64_at(block, 48),
        entries_lba: u64_at(block, 72),
        entry_count: u32_at(block, 80),
        entry_size: u32_at(block, 84),
        entries_crc32: u32_at(block, 88),
    };
    let sized = header.entry_size >= ENTRY_MIN && header.entry_size % 8 == 0;
    let placed = header.my_lba == lba && header.entries_lba != lba;
    (sized && placed).then_some(header)
}

impl Header {
    /// The size of the partition entry array in bytes.
    pub fn entries_size(&self) -> u64 {
        u64::from(self.entry_count) * u64::from(self.entry_size)
    }

    /// The partitions in `entries`, the partition entry array, when its
    /// CRC32 matches the header's: those with a non-zero type GUID.
    pub fn partitions(&self, entries: &[u8]) -> Option<Vec<Partition>> {
        let size = usize::try_from(self.entries_size()).ok()?;
        if entries.len() < size || crc32(&entries[..size]) != self.entries_crc32 {
            return None;
        }
        let chunks = entries[..size].chunks_exact(self.entry_size as usize);
        let found = chunks.enumerate().filter_map(|(i, entry)| {
            let type_guid: [u8; 16] = entry[..16].try_into().expect("16 bytes");
            if type_guid == [0; 16] {
                return None;
            }
            let units = (0..NAME_UNITS)
                .map(|u| u16::from_le_bytes([entry[NAME_AT + 2 * u], entry[NAME_AT + 2 * u + 1]]));
            let name = char::decode_utf16(units.take_while(|&u| u != 0))
                .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect();
            Some(Partition {
                index: i as u32 + 1,
                type_guid,
                guid: entry[16..32].try_into().expect("16 bytes"),
                first_lba: u64_at(entry, 32),
                last_lba: u64_at(entry, 40),
                attributes: u64_at(entry, 48),
                name,
            })
        });
        Some(found.collect())
    }
}

/// `guid`, stored as UEFI stores GUIDs, in lowercase with hyphens: its
/// first three fields are little-endian.
pub fn guid_string(guid: &[u8; 16]) -> String {
    let mut text = String::new();
    let fields: [&[u8]; 5] = [
        &guid[..4],
        &guid[4..6],
        &guid[6..8],
        &guid[8..10],
        &guid[10..],
    ];
    for (n, field) in fields.iter().enumerate() {
        if n > 0 {
            text.push('-');
        }
        let reversed = n < 3;
        for i in 0..field.len() {
            let byte = if reversed {
                field[field.len() - 1 - i]
            } else {
                field[i]
            };
            let _ = write!(text, "{byte:02x}");
        }
    }
    text
}
