//! Directory entries: the 32-byte short entries, the long-name entries
//! before them, and the names they hold.

use alloc::string::String;
use alloc::vec::Vec;

pub(crate) const ENTRY: usize = 32;
/// Attribute bits.
pub const READ_ONLY: u8 = 0x01;
pub const HIDDEN: u8 = 0x02;
pub const SYSTEM: u8 = 0x04;
pub const VOLUME_ID: u8 = 0x08;
pub const DIRECTORY: u8 = 0x10;
pub const ARCHIVE: u8 = 0x20;
pub(crate) const LONG_NAME: u8 = READ_ONLY | HIDDEN | SYSTEM | VOLUME_ID;
/// The first byte of a free entry, and of the free entry that ends the
/// directory.
pub(crate) const FREE: u8 = 0xE5;
pub(crate) const END: u8 = 0x00;
/// Characters of a name in one long-name entry, and the most entries
/// one name takes.
pub(crate) const PER_ENTRY: usize = 13;
pub(crate) const NAME_UNITS_MAX: usize = 255;
/// Where the 13 UTF-16 code units of a long-name entry lie.
pub(crate) const UNITS: [usize; PER_ENTRY] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
/// The case flags of `NTRes`: a lowercase base and a lowercase extension.
const LOWER_BASE: u8 = 0x08;
const LOWER_EXT: u8 = 0x10;

/// A FAT date and time, as the volume stores them: local time, in
/// 2-second steps plus hundredths for creation times.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    pub date: u16,
    pub time: u16,
    pub hundredths: u8,
}

impl Stamp {
    /// The stamp of a local date and time, clamped to 1980–2107.
    pub fn new(
        year: u32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
        ms: u32,
    ) -> Stamp {
        let year = year.clamp(1980, 2107) - 1980;
        Stamp {
            date: (year << 9 | month << 5 | day) as u16,
            time: (hour << 11 | minute << 5 | (second / 2)) as u16,
            hundredths: ((second % 2) * 100 + ms / 10) as u8,
        }
    }

    /// Year, month, day, hour, minute, second and millisecond.
    pub fn parts(&self) -> (u32, u32, u32, u32, u32, u32, u32) {
        let (d, t) = (u32::from(self.date), u32::from(self.time));
        let extra = u32::from(self.hundredths.min(199));
        (
            1980 + (d >> 9),
            (d >> 5 & 0xF).max(1),
            (d & 0x1F).max(1),
            t >> 11,
            t >> 5 & 0x3F,
            (t & 0x1F) * 2 + extra / 100,
            extra % 100 * 10,
        )
    }
}

/// A short directory entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Short {
    pub name: [u8; 11],
    pub attributes: u8,
    pub case: u8,
    pub created: Stamp,
    pub accessed: u16,
    pub modified: Stamp,
    pub cluster: u32,
    pub size: u32,
}

impl Short {
    pub(crate) fn parse(b: &[u8]) -> Short {
        let u16_at = |at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
        let mut name = [0; 11];
        name.copy_from_slice(&b[..11]);
        if name[0] == 0x05 {
            name[0] = FREE;
        }
        Short {
            name,
            attributes: b[11],
            case: b[12],
            created: Stamp {
                date: u16_at(16),
                time: u16_at(14),
                hundredths: b[13],
            },
            accessed: u16_at(18),
            modified: Stamp {
                date: u16_at(24),
                time: u16_at(22),
                hundredths: 0,
            },
            cluster: u32::from(u16_at(20)) << 16 | u32::from(u16_at(26)),
            size: u32::from_le_bytes([b[28], b[29], b[30], b[31]]),
        }
    }

    pub(crate) fn encode(&self) -> [u8; ENTRY] {
        let mut b = [0u8; ENTRY];
        b[..11].copy_from_slice(&self.name);
        if b[0] == FREE {
            b[0] = 0x05;
        }
        b[11] = self.attributes;
        b[12] = self.case;
        b[13] = self.created.hundredths;
        b[14..16].copy_from_slice(&self.created.time.to_le_bytes());
        b[16..18].copy_from_slice(&self.created.date.to_le_bytes());
        b[18..20].copy_from_slice(&self.accessed.to_le_bytes());
        b[20..22].copy_from_slice(&((self.cluster >> 16) as u16).to_le_bytes());
        b[22..24].copy_from_slice(&self.modified.time.to_le_bytes());
        b[24..26].copy_from_slice(&self.modified.date.to_le_bytes());
        b[26..28].copy_from_slice(&(self.cluster as u16).to_le_bytes());
        b[28..32].copy_from_slice(&self.size.to_le_bytes());
        b
    }

    pub(crate) fn is_directory(&self) -> bool {
        self.attributes & DIRECTORY != 0
    }

    /// The name as `BASE.EXT`, in the case its `NTRes` flags give.
    pub(crate) fn display(&self) -> String {
        let part = |bytes: &[u8], lower: bool| -> String {
            let text = bytes
                .iter()
                .rposition(|&c| c != b' ')
                .map_or(&bytes[..0], |end| &bytes[..=end]);
            text.iter()
                .map(|&c| {
                    let c = if c < 0x80 { c as char } else { '\u{FFFD}' };
                    if lower { c.to_ascii_lowercase() } else { c }
                })
                .collect()
        };
        let mut name = part(&self.name[..8], self.case & LOWER_BASE != 0);
        let ext = part(&self.name[8..], self.case & LOWER_EXT != 0);
        if !ext.is_empty() {
            name.push('.');
            name.push_str(&ext);
        }
        name
    }
}

/// What a 32-byte slot of a directory holds.
pub(crate) enum Slot {
    /// The end of the directory: this slot and all after it are free.
    End,
    Free,
    /// A long-name entry: its order number, whether it is the last
    /// (first stored), its checksum and its 13 code units.
    Long {
        order: u8,
        last: bool,
        checksum: u8,
        units: [u16; PER_ENTRY],
    },
    Short(Short),
}

impl Slot {
    pub(crate) fn parse(b: &[u8]) -> Slot {
        match b[0] {
            END => Slot::End,
            FREE => Slot::Free,
            first if b[11] & 0x3F == LONG_NAME => {
                let mut units = [0u16; PER_ENTRY];
                for (unit, &at) in units.iter_mut().zip(&UNITS) {
                    *unit = u16::from_le_bytes([b[at], b[at + 1]]);
                }
                Slot::Long {
                    order: first & 0x3F,
                    last: first & 0x40 != 0,
                    checksum: b[13],
                    units,
                }
            }
            _ => Slot::Short(Short::parse(b)),
        }
    }
}

/// The checksum of a short name that its long-name entries carry.
pub(crate) fn checksum(name: &[u8; 11]) -> u8 {
    name.iter()
        .fold(0u8, |sum, &c| sum.rotate_right(1).wrapping_add(c))
}

/// Collects the long-name entries before a short entry, as the slots of
/// a directory are read in order.
#[derive(Default)]
pub(crate) struct LongName {
    units: Vec<u16>,
    expected: u8,
    checksum: u8,
    /// The slot index where the long name starts.
    pub start: u32,
}

impl LongName {
    pub(crate) fn reset(&mut self) {
        self.units.clear();
        self.expected = 0;
    }

    /// Takes the long-name entry at slot `index`.
    pub(crate) fn push(
        &mut self,
        index: u32,
        order: u8,
        last: bool,
        checksum: u8,
        units: &[u16; PER_ENTRY],
    ) {
        if last {
            self.reset();
            if order == 0 || usize::from(order) * PER_ENTRY > NAME_UNITS_MAX + PER_ENTRY {
                return;
            }
            self.units = alloc::vec![0; usize::from(order) * PER_ENTRY];
            self.expected = order;
            self.checksum = checksum;
            self.start = index;
        } else if self.expected == 0 || order != self.expected || checksum != self.checksum {
            self.reset();
            return;
        }
        let at = (usize::from(order) - 1) * PER_ENTRY;
        self.units[at..at + PER_ENTRY].copy_from_slice(units);
        self.expected = order - 1;
    }

    /// The long name of the short entry `short`, when the entries before
    /// it make one, and its first slot.
    pub(crate) fn take(&mut self, short: &Short) -> Option<(String, u32)> {
        let complete = self.expected == 0 && !self.units.is_empty();
        let matches = complete && self.checksum == checksum(&short.name);
        let units = core::mem::take(&mut self.units);
        self.reset();
        if !matches {
            return None;
        }
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        let name: String = char::decode_utf16(units[..end].iter().copied())
            .map(|c| c.unwrap_or('\u{FFFD}'))
            .collect();
        (!name.is_empty()).then_some((name, self.start))
    }
}

/// Whether two names are the same name on FAT, which ignores case.
pub(crate) fn same(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_uppercase)
        .eq(b.chars().flat_map(char::to_uppercase))
}
