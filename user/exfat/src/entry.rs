//! Directory entries: the entry sets of files and directories, a file
//! entry, a stream extension and file name entries, with their checksum,
//! and the critical primary entries of the root directory.

#[cfg(test)]
use crate::upcase::Upcase;
use alloc::string::String;
use alloc::vec::Vec;

/// The size of a directory entry.
pub const ENTRY: usize = 32;
/// Entry types, with the in-use bit set.
pub const END: u8 = 0x00;
pub const BITMAP: u8 = 0x81;
pub const UPCASE: u8 = 0x82;
pub const LABEL: u8 = 0x83;
pub const FILE: u8 = 0x85;
pub const STREAM: u8 = 0xC0;
pub const NAME: u8 = 0xC1;
/// The UTF-16 code units of a file name entry.
pub const NAME_UNITS: usize = 15;
/// The most UTF-16 code units of a name.
pub const NAME_MAX: usize = 255;
/// The attributes of a directory, and of a file changed since it was
/// last archived.
pub const DIRECTORY: u16 = 0x10;
pub const ARCHIVE: u16 = 0x20;
/// The stream extension's flags: clusters may be allocated, and they lie
/// in a row without FAT entries.
pub const ALLOCATION_POSSIBLE: u8 = 1 << 0;
pub const NO_FAT_CHAIN: u8 = 1 << 1;

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// The checksum of the entry set `set`, which leaves out the checksum's
/// own bytes in the file entry.
pub fn set_checksum(set: &[u8]) -> u16 {
    let mut sum = 0u16;
    for (i, &byte) in set.iter().enumerate() {
        if i == 2 || i == 3 {
            continue;
        }
        sum = sum.rotate_right(1).wrapping_add(u16::from(byte));
    }
    sum
}

/// A timestamp: the local date and time in the DOS layout, its 10 ms
/// increment, and its offset from UTC, valid when bit 7 is set, in steps
/// of 15 minutes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    pub timestamp: u32,
    pub increment: u8,
    pub utc_offset: u8,
}

impl Stamp {
    /// The offset from UTC in minutes, when it is valid.
    pub fn offset_minutes(&self) -> Option<i32> {
        if self.utc_offset & 0x80 == 0 {
            return None;
        }
        // A signed 7-bit value.
        let steps = i32::from(((self.utc_offset & 0x7F) << 1) as i8 >> 1);
        Some(steps * 15)
    }
}

/// What the entry set of a file or directory holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Set {
    pub attributes: u16,
    pub created: Stamp,
    pub modified: Stamp,
    pub accessed: Stamp,
    pub flags: u8,
    pub valid_size: u64,
    pub first_cluster: u32,
    pub size: u64,
    pub name: Vec<u16>,
}

impl Set {
    pub fn is_directory(&self) -> bool {
        self.attributes & DIRECTORY != 0
    }

    pub fn contiguous(&self) -> bool {
        self.flags & NO_FAT_CHAIN != 0
    }

    pub fn name(&self) -> String {
        String::from_utf16_lossy(&self.name)
    }

    /// How many entries the set takes.
    #[cfg(test)]
    pub fn entries(&self) -> usize {
        entries_for(self.name.len())
    }

    /// Writes the fields of the set's file entry and stream extension
    /// into `set`, the entries of a set, and leaves the rest as it is.
    pub fn write_fields(&self, set: &mut [u8]) {
        set[4..6].copy_from_slice(&self.attributes.to_le_bytes());
        let stamps = [(8, 20, 22, self.created), (12, 21, 23, self.modified)];
        for (at, increment_at, offset_at, stamp) in stamps {
            set[at..at + 4].copy_from_slice(&stamp.timestamp.to_le_bytes());
            set[increment_at] = stamp.increment;
            set[offset_at] = stamp.utc_offset;
        }
        set[16..20].copy_from_slice(&self.accessed.timestamp.to_le_bytes());
        set[24] = self.accessed.utc_offset;
        let s = &mut set[ENTRY..2 * ENTRY];
        s[1] = self.flags;
        s[8..16].copy_from_slice(&self.valid_size.to_le_bytes());
        s[20..24].copy_from_slice(&self.first_cluster.to_le_bytes());
        s[24..32].copy_from_slice(&self.size.to_le_bytes());
    }

    /// The entries of the set, with its checksum and the name's hash.
    #[cfg(test)]
    pub fn encode(&self, upcase: &Upcase) -> Vec<u8> {
        let count = self.entries();
        let mut set = alloc::vec![0u8; count * ENTRY];
        set[0] = FILE;
        set[1] = (count - 1) as u8;
        set[ENTRY] = STREAM;
        set[ENTRY + 3] = self.name.len() as u8;
        set[ENTRY + 4..ENTRY + 6].copy_from_slice(&upcase.hash(&self.name).to_le_bytes());
        self.write_fields(&mut set);
        for (i, chunk) in self.name.chunks(NAME_UNITS).enumerate() {
            let n = &mut set[(2 + i) * ENTRY..(3 + i) * ENTRY];
            n[0] = NAME;
            for (j, unit) in chunk.iter().enumerate() {
                n[2 + 2 * j..4 + 2 * j].copy_from_slice(&unit.to_le_bytes());
            }
        }
        let sum = set_checksum(&set);
        set[2..4].copy_from_slice(&sum.to_le_bytes());
        set
    }
}

/// How many entries the set of a name of `units` code units takes.
pub fn entries_for(units: usize) -> usize {
    2 + units.div_ceil(NAME_UNITS).max(1)
}

/// The entry set that starts with the file entry `set`, which holds the
/// set's entries, or `None` when it is damaged: a wrong count, entry
/// type, name length or checksum.
pub fn parse(set: &[u8]) -> Option<Set> {
    if set.len() < 3 * ENTRY || set[0] != FILE {
        return None;
    }
    let count = usize::from(set[1]) + 1;
    if count < 3 || set.len() < count * ENTRY {
        return None;
    }
    let set = &set[..count * ENTRY];
    if u16_at(set, 2) != set_checksum(set) {
        return None;
    }
    let stream = &set[ENTRY..2 * ENTRY];
    if stream[0] != STREAM {
        return None;
    }
    let units = usize::from(stream[3]);
    if units == 0 || entries_for(units) > count {
        return None;
    }
    let mut name = Vec::with_capacity(units);
    for i in 0..units.div_ceil(NAME_UNITS) {
        let entry = &set[(2 + i) * ENTRY..(3 + i) * ENTRY];
        if entry[0] != NAME {
            return None;
        }
        let take = (units - i * NAME_UNITS).min(NAME_UNITS);
        name.extend((0..take).map(|j| u16_at(entry, 2 + 2 * j)));
    }
    let stamp = |at: usize, increment: u8, offset: u8| Stamp {
        timestamp: u32_at(set, at),
        increment,
        utc_offset: offset,
    };
    Some(Set {
        attributes: u16_at(set, 4),
        created: stamp(8, set[20], set[22]),
        modified: stamp(12, set[21], set[23]),
        accessed: stamp(16, 0, set[24]),
        flags: stream[1],
        valid_size: u64_at(stream, 8),
        first_cluster: u32_at(stream, 20),
        size: u64_at(stream, 24),
        name,
    })
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
