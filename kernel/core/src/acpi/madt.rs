//! Reading the MADT: the interrupt controllers and their NMI inputs.

use super::{HEADER_SIZE, u32_at, u64_at};

/// The processor UID that stands for every processor.
pub const ALL_PROCESSORS: u32 = u32::MAX;

/// A MADT entry the kernel uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A processor's local APIC or x2APIC.
    LocalApic {
        uid: u32,
        apic_id: u32,
        flags: u32,
    },
    IoApic {
        id: u8,
        address: u32,
        gsi_base: u32,
    },
    /// A local APIC input wired to NMI, for the processor `uid`, which
    /// may be `ALL_PROCESSORS`.
    LocalNmi {
        uid: u32,
        flags: u16,
        lint: u8,
    },
    /// The 64-bit physical address of the local APICs.
    LocalApicAddress(u64),
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn entry(kind: u8, e: &[u8]) -> Option<Entry> {
    Some(match kind {
        0 => Entry::LocalApic {
            uid: u32::from(*e.get(2)?),
            apic_id: u32::from(*e.get(3)?),
            flags: u32_at(e, 4)?,
        },
        1 => Entry::IoApic {
            id: *e.get(2)?,
            address: u32_at(e, 4)?,
            gsi_base: u32_at(e, 8)?,
        },
        4 => Entry::LocalNmi {
            uid: match *e.get(2)? {
                0xFF => ALL_PROCESSORS,
                uid => u32::from(uid),
            },
            flags: u16_at(e, 3)?,
            lint: *e.get(5)?,
        },
        5 => Entry::LocalApicAddress(u64_at(e, 4)?),
        9 => Entry::LocalApic {
            apic_id: u32_at(e, 4)?,
            flags: u32_at(e, 8)?,
            uid: u32_at(e, 12)?,
        },
        0x0A => Entry::LocalNmi {
            flags: u16_at(e, 2)?,
            uid: u32_at(e, 4)?,
            lint: *e.get(8)?,
        },
        _ => return None,
    })
}

/// The entries of the MADT `madt`, a table whose length and checksum are
/// checked. Entries of other types are skipped, and the walk stops at an
/// entry that does not fit in the table.
pub fn entries(madt: &[u8]) -> impl Iterator<Item = Entry> + '_ {
    let mut rest = madt.get(HEADER_SIZE + 8..).unwrap_or(&[]);
    core::iter::from_fn(move || {
        loop {
            let (&kind, &len) = (rest.first()?, rest.get(1)?);
            let len = usize::from(len);
            if len < 2 || len > rest.len() {
                return None;
            }
            let (e, next) = rest.split_at(len);
            rest = next;
            if let Some(entry) = entry(kind, e) {
                return Some(entry);
            }
        }
    })
}

/// The physical address of the local APICs.
pub fn local_apic_address(madt: &[u8]) -> Option<u64> {
    let base = u64::from(u32_at(madt, HEADER_SIZE)?);
    Some(
        entries(madt)
            .find_map(|e| match e {
                Entry::LocalApicAddress(address) => Some(address),
                _ => None,
            })
            .unwrap_or(base),
    )
}

/// The local APIC inputs wired to NMI on the processor whose APIC ID is
/// `apic_id`, with their MPS INTI flags.
pub fn nmi_inputs(madt: &[u8], apic_id: u32) -> impl Iterator<Item = (u8, u16)> + '_ {
    let uid = entries(madt).find_map(|e| match e {
        Entry::LocalApic {
            uid, apic_id: id, ..
        } if id == apic_id => Some(uid),
        _ => None,
    });
    entries(madt).filter_map(move |e| match e {
        Entry::LocalNmi {
            uid: u,
            flags,
            lint,
        } if u == ALL_PROCESSORS || Some(u) == uid => Some((lint, flags)),
        _ => None,
    })
}

/// The flag of a local APIC entry whose processor is enabled.
pub const ENABLED: u32 = 1 << 0;

/// The APIC IDs of the processors whose local APIC or x2APIC entry has
/// `ENABLED` set, in table order, each once.
pub fn enabled_processors(madt: &[u8]) -> impl Iterator<Item = u32> + '_ {
    let enabled = move |e| match e {
        Entry::LocalApic { apic_id, flags, .. } if flags & ENABLED != 0 => Some(apic_id),
        _ => None,
    };
    entries(madt)
        .filter_map(enabled)
        .enumerate()
        .filter(move |&(n, id)| !entries(madt).filter_map(enabled).take(n).any(|i| i == id))
        .map(|(_, id)| id)
}

/// Whether MPS INTI `flags` select an active-low input.
pub fn active_low(flags: u16) -> bool {
    flags & 0b11 == 0b11
}

/// Whether MPS INTI `flags` select a level-triggered input.
pub fn level_triggered(flags: u16) -> bool {
    flags >> 2 & 0b11 == 0b11
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn madt(entries: &[&[u8]]) -> Vec<u8> {
        let mut t = std::vec![0u8; HEADER_SIZE];
        t[..4].copy_from_slice(b"APIC");
        t.extend_from_slice(&0xFEE0_0000u32.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        for e in entries {
            t.extend_from_slice(e);
        }
        t
    }

    #[test]
    fn reads_entries() {
        let t = madt(&[
            &[0, 8, 1, 0, 1, 0, 0, 0],
            &[1, 12, 2, 0, 0, 0, 0xC0, 0xFE, 0, 0, 0, 0],
            &[2, 10, 0, 0, 2, 0, 0, 0, 0, 0],
            &[4, 6, 0xFF, 0x05, 0x00, 1],
            &[9, 16, 0, 0, 7, 0, 0, 0, 1, 0, 0, 0, 9, 0, 0, 0],
            &[0x0A, 12, 0x0F, 0, 9, 0, 0, 0, 0, 0, 0, 0],
        ]);
        let all: Vec<Entry> = entries(&t).collect();
        assert_eq!(
            all,
            [
                Entry::LocalApic {
                    uid: 1,
                    apic_id: 0,
                    flags: 1
                },
                Entry::IoApic {
                    id: 2,
                    address: 0xFEC0_0000,
                    gsi_base: 0
                },
                Entry::LocalNmi {
                    uid: ALL_PROCESSORS,
                    flags: 5,
                    lint: 1
                },
                Entry::LocalApic {
                    uid: 9,
                    apic_id: 7,
                    flags: 1
                },
                Entry::LocalNmi {
                    uid: 9,
                    flags: 0xF,
                    lint: 0
                },
            ]
        );
        assert_eq!(local_apic_address(&t), Some(0xFEE0_0000));
        let nmi: Vec<_> = nmi_inputs(&t, 0).collect();
        assert_eq!(nmi, [(1, 5)]);
        let nmi: Vec<_> = nmi_inputs(&t, 7).collect();
        assert_eq!(nmi, [(1, 5), (0, 0xF)]);
        assert!(active_low(0xF) && level_triggered(0xF));
        assert!(!active_low(5) && !level_triggered(5));
    }

    #[test]
    fn lists_enabled_processors_once() {
        let t = madt(&[
            &[0, 8, 0, 0, 1, 0, 0, 0],
            &[0, 8, 1, 2, 2, 0, 0, 0],
            &[0, 8, 2, 3, 0, 0, 0, 0],
            &[9, 16, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0, 4, 0, 0, 0],
            &[9, 16, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 5, 0, 0, 0],
            &[9, 16, 0, 0, 1, 1, 0, 0, 3, 0, 0, 0, 6, 0, 0, 0],
        ]);
        let ids: Vec<u32> = enabled_processors(&t).collect();
        assert_eq!(ids, [0, 0x100, 0x101]);
    }

    #[test]
    fn takes_the_address_override_and_stops_at_bad_entries() {
        let t = madt(&[
            &[5, 12, 0, 0, 0, 0, 0, 0xFE, 1, 0, 0, 0],
            &[1, 1],
            &[0, 8, 0, 0, 1, 0, 0, 0],
        ]);
        assert_eq!(local_apic_address(&t), Some(0x1_FE00_0000));
        assert_eq!(entries(&t).count(), 1);
        assert_eq!(entries(&madt(&[&[1, 12, 0]])).count(), 0);
    }
}
