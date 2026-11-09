//! The critical primary entries of the root directory: the allocation
//! bitmap's, the up-case table's and the volume label's.

use alloc::string::String;
use alloc::vec::Vec;

/// The size of a directory entry.
pub const ENTRY: usize = 32;
/// Entry types, with the in-use bit set.
pub const END: u8 = 0x00;
pub const BITMAP: u8 = 0x81;
pub const UPCASE: u8 = 0x82;
pub const LABEL: u8 = 0x83;

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// The first cluster and length of a critical primary entry of the root
/// directory, such as the allocation bitmap's.
pub fn extent(entry: &[u8]) -> (u32, u64) {
    (u32_at(entry, 20), u64_at(entry, 24))
}

/// The checksum that an up-case table entry gives.
pub fn table_checksum(entry: &[u8]) -> u32 {
    u32_at(entry, 4)
}

/// The volume label of a volume label entry.
pub fn label(entry: &[u8]) -> String {
    let count = usize::from(entry[1]).min(11);
    let units: Vec<u16> = (0..count).map(|i| u16_at(entry, 2 + 2 * i)).collect();
    String::from_utf16_lossy(&units)
}
