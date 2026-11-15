//! The UEFI Memory Attributes Table, which says which pages of the
//! runtime services' images hold code and which hold data.

use crate::memmap::Region;
use crate::memmap::uefi_type::{RUNTIME_SERVICES_CODE, RUNTIME_SERVICES_DATA};
use boot_protocol::{Kind, PAGE_SIZE};

/// The table's GUID in the configuration table, as UEFI stores it.
pub const GUID: [u8; 16] = [
    0x1d, 0x91, 0xfa, 0xdc, 0xeb, 0x26, 0x9f, 0x46, 0xa2, 0x20, 0x38, 0xb7, 0xdc, 0x46, 0x12, 0x20,
];
/// The size of the table's header.
pub const HEADER: usize = 16;
/// The smallest descriptor size, that of `EFI_MEMORY_DESCRIPTOR`.
const DESCRIPTOR_MIN: usize = 40;
/// The memory attributes that mark pages as not executable and as read
/// only.
pub const EFI_MEMORY_XP: u64 = 0x4000;
pub const EFI_MEMORY_RO: u64 = 0x20000;

/// An entry of the table: its first page and number of pages, and its
/// attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub start: u64,
    pub pages: u64,
    pub attribute: u64,
}

impl Entry {
    /// The address after the entry's last page, unless it overflows.
    fn end(&self) -> Option<u64> {
        self.pages.checked_mul(PAGE_SIZE)?.checked_add(self.start)
    }

    /// Whether the entry lies within the firmware range of `pages` pages
    /// at `start`.
    fn within(&self, start: u64, pages: u64) -> bool {
        let end = self.end().unwrap_or(u64::MAX);
        start <= self.start
            && pages
                .checked_mul(PAGE_SIZE)
                .and_then(|n| n.checked_add(start))
                .is_some_and(|e| end <= e)
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// The number of bytes of the table whose header is `header`, when the
/// table may be used: version 1 or 2, with descriptors of at least 40
/// bytes.
pub fn size(header: &[u8; HEADER]) -> Option<usize> {
    let (version, count, size) = (u32_at(header, 0), u32_at(header, 4), u32_at(header, 8));
    if !(1..=2).contains(&version) || (size as usize) < DESCRIPTOR_MIN {
        return None;
    }
    HEADER.checked_add((count as usize).checked_mul(size as usize)?)
}

/// The number of entries in the table whose header is `header`.
pub fn count(header: &[u8; HEADER]) -> usize {
    u32_at(header, 4) as usize
}

/// The entries of `table`, whose header `size` accepted; none when the
/// table is shorter than its header.
pub fn entries(table: &[u8]) -> impl Iterator<Item = Entry> + Clone + '_ {
    let (count, size) = match table.get(..HEADER) {
        Some(h) => (u32_at(h, 4) as usize, (u32_at(h, 8) as usize).max(1)),
        None => (0, 1),
    };
    (0..count)
        .map(move |i| HEADER + i * size)
        .take_while(|&at| at + DESCRIPTOR_MIN <= table.len())
        .map(|at| Entry {
            start: u64_at(table, at + 8),
            pages: u64_at(table, at + 24),
            attribute: u64_at(table, at + 32),
        })
}

/// Whether every entry is page-aligned and lies within a range of the
/// firmware map `firmware`, given as start, page count and UEFI type,
/// that holds runtime services code or data.
pub fn usable(
    mut entries: impl Iterator<Item = Entry>,
    firmware: impl Iterator<Item = (u64, u64, u32)> + Clone,
) -> bool {
    entries.all(|e| {
        e.start.is_multiple_of(PAGE_SIZE)
            && e.end().is_some()
            && firmware.clone().any(|(start, pages, kind)| {
                matches!(kind, RUNTIME_SERVICES_CODE | RUNTIME_SERVICES_DATA)
                    && e.within(start, pages)
            })
    })
}

/// The kind that the pages of `entry` get within runtime services code:
/// data when they are not executable, code when they are read only, and
/// mixed otherwise.
pub fn kind(entry: &Entry) -> Kind {
    if entry.attribute & EFI_MEMORY_XP != 0 {
        Kind::FirmwareRuntime
    } else if entry.attribute & EFI_MEMORY_RO != 0 {
        Kind::FirmwareRuntimeCode
    } else {
        Kind::FirmwareRuntimeMixed
    }
}

/// The regions that `entries`, which `usable` accepted, give to the pages
/// of runtime services code in `firmware`, as `kind` says. They overlay
/// the kind that the code's ranges get without the table.
pub fn regions(
    entries: impl Iterator<Item = Entry>,
    firmware: impl Iterator<Item = (u64, u64, u32)> + Clone,
) -> impl Iterator<Item = Region> {
    entries.filter_map(move |e| {
        let in_code = firmware
            .clone()
            .any(|(start, pages, kind)| kind == RUNTIME_SERVICES_CODE && e.within(start, pages));
        in_code.then(|| Region {
            start: e.start,
            pages: e.pages,
            kind: kind(&e),
        })
    })
}

/// A table of `version` with descriptors of `size` bytes, holding
/// `entries` as UEFI type, start, page count and attributes.
#[cfg(test)]
pub(crate) fn table(
    version: u32,
    size: u32,
    entries: &[(u32, u64, u64, u64)],
) -> alloc::vec::Vec<u8> {
    let mut t = alloc::vec![0u8; HEADER];
    t[0..4].copy_from_slice(&version.to_le_bytes());
    t[4..8].copy_from_slice(&(entries.len() as u32).to_le_bytes());
    t[8..12].copy_from_slice(&size.to_le_bytes());
    for &(kind, start, pages, attribute) in entries {
        let mut d = alloc::vec![0u8; (size as usize).max(DESCRIPTOR_MIN)];
        d[0..4].copy_from_slice(&kind.to_le_bytes());
        d[8..16].copy_from_slice(&start.to_le_bytes());
        d[24..32].copy_from_slice(&pages.to_le_bytes());
        d[32..40].copy_from_slice(&attribute.to_le_bytes());
        t.extend(&d[..size as usize]);
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmdline::guid_text;
    use alloc::vec::Vec;

    const P: u64 = PAGE_SIZE;

    fn header(t: &[u8]) -> [u8; HEADER] {
        t[..HEADER].try_into().unwrap()
    }

    /// A runtime code range at pages 16 to 31, and a data range at pages
    /// 32 to 35.
    fn firmware() -> [(u64, u64, u32); 3] {
        [(0, 16, 7), (16 * P, 16, 5), (32 * P, 4, 6)]
    }

    #[test]
    fn knows_the_guid() {
        assert_eq!(guid_text(&GUID), "dcfa911d-26eb-469f-a220-38b7dc461220");
    }

    #[test]
    fn reads_versions_1_and_2_only() {
        let entries = [(5, 16 * P, 2, EFI_MEMORY_RO), (5, 18 * P, 3, EFI_MEMORY_XP)];
        for (version, size, usable) in
            [(1, 48, true), (2, 40, true), (3, 48, false), (1, 32, false)]
        {
            let t = table(version, size, &entries);
            assert_eq!(
                size_of_table(&t),
                usable.then_some(HEADER + 2 * size as usize),
                "version {version}, size {size}"
            );
        }
        let t = table(1, 48, &entries);
        assert_eq!(count(&header(&t)), 2);
        assert_eq!(
            super::entries(&t).collect::<Vec<_>>(),
            [
                Entry {
                    start: 16 * P,
                    pages: 2,
                    attribute: EFI_MEMORY_RO
                },
                Entry {
                    start: 18 * P,
                    pages: 3,
                    attribute: EFI_MEMORY_XP
                }
            ]
        );
    }

    #[test]
    fn stops_at_the_end_of_a_short_table() {
        let mut t = table(1, 48, &[(5, 16 * P, 1, 0), (5, 17 * P, 1, 0)]);
        t.truncate(HEADER + 48 + 39);
        assert_eq!(super::entries(&t).count(), 1);
        assert_eq!(super::entries(&t[..8]).count(), 0);
    }

    fn size_of_table(t: &[u8]) -> Option<usize> {
        size(&header(t))
    }

    #[test]
    fn needs_aligned_entries_in_runtime_ranges() {
        let entry = |start, pages| Entry {
            start,
            pages,
            attribute: 0,
        };
        let fw = firmware();
        let check = |entries: &[Entry]| usable(entries.iter().copied(), fw.into_iter());
        assert!(check(&[entry(16 * P, 16), entry(32 * P, 4)]));
        assert!(check(&[]));
        assert!(!check(&[entry(16 * P + 8, 1)]), "unaligned");
        assert!(!check(&[entry(30 * P, 4)]), "across ranges");
        assert!(!check(&[entry(2 * P, 1)]), "conventional");
        assert!(!check(&[entry(16 * P, u64::MAX)]), "overflowing");
        assert!(!check(&[entry(16 * P, 1), entry(40 * P, 1)]), "one outside");
    }

    #[test]
    fn gives_code_pages_their_kinds() {
        let entries = [
            Entry {
                start: 16 * P,
                pages: 2,
                attribute: EFI_MEMORY_RO,
            },
            Entry {
                start: 18 * P,
                pages: 3,
                attribute: EFI_MEMORY_XP | EFI_MEMORY_RO,
            },
            Entry {
                start: 21 * P,
                pages: 1,
                attribute: 0,
            },
            Entry {
                start: 32 * P,
                pages: 4,
                attribute: EFI_MEMORY_XP,
            },
        ];
        let regions: Vec<Region> =
            regions(entries.iter().copied(), firmware().into_iter()).collect();
        assert_eq!(
            regions,
            [
                Region {
                    start: 16 * P,
                    pages: 2,
                    kind: Kind::FirmwareRuntimeCode
                },
                Region {
                    start: 18 * P,
                    pages: 3,
                    kind: Kind::FirmwareRuntime
                },
                Region {
                    start: 21 * P,
                    pages: 1,
                    kind: Kind::FirmwareRuntimeMixed
                },
            ]
        );
    }
}
