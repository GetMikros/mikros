//! Directory entries: the 32-byte short entries and the names they
//! hold. Long-name entries are skipped.

use alloc::string::String;

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
    /// A long-name entry.
    Long,
    Short(Short),
}

impl Slot {
    pub(crate) fn parse(b: &[u8]) -> Slot {
        match b[0] {
            END => Slot::End,
            FREE => Slot::Free,
            _ if b[11] & 0x3F == LONG_NAME => Slot::Long,
            _ => Slot::Short(Short::parse(b)),
        }
    }
}

/// Whether two names are the same name on FAT, which ignores case.
pub(crate) fn same(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_uppercase)
        .eq(b.chars().flat_map(char::to_uppercase))
}
