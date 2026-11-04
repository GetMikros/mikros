//! Reading the IVRS table: the AMD-Vi units, the devices each covers and
//! the memory that firmware reserves for devices.

use super::{HEADER_SIZE, u64_at};
use alloc::vec::Vec;
use core::ops::RangeInclusive;

/// The first block follows the IVinfo field and eight reserved bytes.
const BLOCKS: usize = HEADER_SIZE + 12;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

/// An IOMMU, from the IVHD block of the highest type the table gives for
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    /// The block type: 0x10, 0x11 or 0x40.
    pub kind: u8,
    pub segment: u16,
    /// The IOMMU's own device ID, and the offset of its capability block
    /// in its configuration space.
    pub device: u16,
    pub capability: u16,
    /// The physical address of its registers.
    pub base: u64,
    /// The device IDs whose DMA and interrupts it handles.
    pub devices: Vec<RangeInclusive<u16>>,
    /// The I/O APICs behind it, by ID, with the device ID their
    /// interrupts carry.
    pub io_apics: Vec<(u8, u16)>,
    /// Devices whose DMA reaches the unit with another device ID, with
    /// that ID.
    pub aliases: Vec<(RangeInclusive<u16>, u16)>,
}

impl Unit {
    /// The device ID the DMA of `device` reaches the unit with, when it is
    /// another one.
    pub fn alias(&self, device: u16) -> Option<u16> {
        self.aliases
            .iter()
            .find(|(r, _)| r.contains(&device))
            .map(|&(_, alias)| alias)
            .filter(|&alias| alias != device)
    }

    /// Whether the unit handles the device with requester ID `rid`.
    pub fn covers(&self, rid: u64) -> bool {
        let segment = (rid >> 16) as u16;
        let device = rid as u16;
        segment == self.segment && self.devices.iter().any(|r| r.contains(&device))
    }
}

/// The device entries of an IVHD block from `at`, added to `unit`. A
/// range opened by a start entry ends at the next end entry.
fn device_entries(bytes: &[u8], unit: &mut Unit) {
    let mut at = 0;
    let mut start = None;
    while let Some(&kind) = bytes.get(at) {
        let len = match kind {
            0..64 => 4,
            64..128 => 8,
            // A variable-length entry gives its UID length at byte 21.
            _ => match bytes.get(at + 21) {
                Some(&uid) => 22 + usize::from(uid),
                None => break,
            },
        };
        let Some(entry) = bytes.get(at..at + len) else {
            break;
        };
        let id = u16_at(entry, 1).unwrap_or(0);
        // Alias entries give the ID the devices' DMA arrives with.
        let alias = u16_at(entry, 5).filter(|_| kind == 66 || kind == 67);
        match kind {
            1 => unit.devices.push(0..=u16::MAX),
            2 | 66 | 70 => {
                unit.devices.push(id..=id);
                if let Some(alias) = alias {
                    unit.aliases.push((id..=id, alias));
                }
            }
            3 | 67 | 71 => start = Some((id, alias)),
            4 => {
                if let Some((first, alias)) = start.take() {
                    unit.devices.push(first..=id);
                    if let Some(alias) = alias {
                        unit.aliases.push((first..=id, alias));
                    }
                }
            }
            72 => {
                let device = u16_at(entry, 5).unwrap_or(0);
                // Variety 1 is an I/O APIC; its handle is the APIC ID.
                if entry[7] == 1 {
                    unit.io_apics.push((entry[4], device));
                }
                unit.devices.push(device..=device);
            }
            _ => {}
        }
        at += len;
    }
}

/// The units of `ivrs`. Where blocks of several types describe the IOMMU
/// at one base address, the one of the highest type is used.
pub fn units(ivrs: &[u8]) -> Vec<Unit> {
    let mut units: Vec<Unit> = Vec::new();
    let mut at = BLOCKS;
    while let (Some(&kind), Some(len)) = (ivrs.get(at), u16_at(ivrs, at + 2)) {
        let len = usize::from(len);
        let Some(b) = ivrs.get(at..at + len).filter(|_| len >= 4) else {
            break;
        };
        at += len;
        let entries = match kind {
            0x10 => 24,
            0x11 | 0x40 => 40,
            _ => continue,
        };
        let (Some(device), Some(capability), Some(base), Some(segment)) =
            (u16_at(b, 4), u16_at(b, 6), u64_at(b, 8), u16_at(b, 16))
        else {
            continue;
        };
        let mut unit = Unit {
            kind,
            segment,
            device,
            capability,
            base,
            devices: Vec::new(),
            io_apics: Vec::new(),
            aliases: Vec::new(),
        };
        device_entries(b.get(entries..).unwrap_or(&[]), &mut unit);
        match units.iter_mut().find(|u| u.base == base) {
            Some(old) if old.kind < kind => *old = unit,
            Some(_) => {}
            None => units.push(unit),
        }
    }
    units
}

/// The devices an IVMD block names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Devices {
    All,
    Range(RangeInclusive<u16>),
}

/// Memory that firmware reserves for devices (IVMD), with the access
/// they need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reserved {
    pub segment: u16,
    pub devices: Devices,
    pub base: u64,
    pub length: u64,
    pub read: bool,
    pub write: bool,
}

/// The reserved memory of `ivrs`.
pub fn reserved(ivrs: &[u8]) -> Vec<Reserved> {
    let mut out = Vec::new();
    let mut at = BLOCKS;
    while let (Some(&kind), Some(len)) = (ivrs.get(at), u16_at(ivrs, at + 2)) {
        let len = usize::from(len);
        let Some(b) = ivrs.get(at..at + len).filter(|_| len >= 4) else {
            break;
        };
        at += len;
        let (Some(first), Some(aux), Some(base), Some(length)) =
            (u16_at(b, 4), u16_at(b, 6), u64_at(b, 16), u64_at(b, 24))
        else {
            continue;
        };
        let devices = match kind {
            0x20 => Devices::All,
            0x21 => Devices::Range(first..=first),
            0x22 => Devices::Range(first..=aux),
            _ => continue,
        };
        let flags = b[1];
        out.push(Reserved {
            // These blocks name devices of segment 0.
            segment: 0,
            devices,
            base,
            length,
            read: flags & 1 << 1 != 0,
            write: flags & 1 << 2 != 0,
        });
    }
    out
}

#[cfg(test)]
mod tests;
