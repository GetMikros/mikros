//! Reading the DMAR table: the VT-d remapping units, the memory that
//! firmware reserves for devices, and the devices each one covers.

use super::{HEADER_SIZE, u64_at};
use alloc::vec::Vec;
use core::ops::RangeInclusive;

/// DMAR flags: interrupt remapping is supported, and firmware asks for
/// DMA translation to stay enabled from boot on.
pub const INTR_REMAP: u8 = 1 << 0;
pub const DMA_CTRL_PLATFORM_OPT_IN: u8 = 1 << 2;

/// The first remapping structure follows the host address width, the
/// flags and ten reserved bytes.
const STRUCTURES: usize = HEADER_SIZE + 12;

/// The requester ID of PCI function `function` of `device` on `bus` in
/// `segment`.
pub fn requester_id(segment: u16, bus: u8, device: u8, function: u8) -> u64 {
    u64::from(segment) << 16 | u64::from(bus) << 8 | u64::from(device) << 3 | u64::from(function)
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

/// The DMAR flags of `dmar`.
pub fn flags(dmar: &[u8]) -> u8 {
    dmar.get(HEADER_SIZE + 1).copied().unwrap_or(0)
}

/// A device scope entry, before its path is followed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scope<'a> {
    /// 1 for a PCI endpoint, 2 for a PCI bridge and everything below it,
    /// 3 for an I/O APIC and 4 for an HPET.
    pub kind: u8,
    /// The I/O APIC or HPET ID for those kinds.
    pub enumeration_id: u8,
    pub start_bus: u8,
    /// Pairs of device and function numbers, from `start_bus` down.
    pub path: &'a [u8],
}

/// What a device scope names, with its path followed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// One PCI function, by requester ID.
    Function(u64),
    /// A PCI bridge, by requester ID, and the buses below it.
    Bridge(u64, RangeInclusive<u8>),
    /// An I/O APIC, by ID, and the requester ID its interrupts carry.
    IoApic(u8, u64),
    /// Something the kernel does not use.
    Other,
}

/// A configuration space byte register reader: segment, bus, device,
/// function and offset.
pub trait Config {
    fn read8(&self, segment: u16, bus: u8, device: u8, function: u8, offset: u16) -> u8;
}

/// The offsets of a PCI bridge's secondary and subordinate bus numbers,
/// of the status register's low byte and of the capability pointer.
const SECONDARY_BUS: u16 = 0x19;
const SUBORDINATE_BUS: u16 = 0x1A;
const STATUS: u16 = 0x06;
const CAPABILITIES: u16 = 0x34;
/// The PCI Express capability, and its device type for a PCI Express to
/// PCI or PCI-X bridge.
const PCI_EXPRESS: u8 = 0x10;
const PCIE_TO_PCI: u8 = 0x7;

/// Whether the function is a PCI Express to PCI or PCI-X bridge.
fn pcie_to_pci(config: &impl Config, segment: u16, bus: u8, device: u8, function: u8) -> bool {
    let read = |offset| config.read8(segment, bus, device, function, offset);
    if read(STATUS) & 1 << 4 == 0 {
        return false;
    }
    let mut at = read(CAPABILITIES) & 0xFC;
    // A list longer than the space it lives in is broken.
    for _ in 0..48 {
        if at == 0 {
            return false;
        }
        if read(u16::from(at)) == PCI_EXPRESS {
            return read(u16::from(at) + 2) >> 4 & 0xF == PCIE_TO_PCI;
        }
        at = read(u16::from(at) + 1) & 0xFC;
    }
    false
}

impl Scope<'_> {
    /// Follows the path through the bridges it names, reading their bus
    /// numbers through `config`, and gives what the scope names, or
    /// `None` for a malformed path.
    pub fn resolve(&self, segment: u16, config: &impl Config) -> Option<Target> {
        let (last, bridges) = self.path.as_chunks::<2>().0.split_last()?;
        let mut bus = self.start_bus;
        for &[device, function] in bridges {
            bus = config.read8(segment, bus, device, function, SECONDARY_BUS);
        }
        let last = *last;
        let [device, function] = last;
        let rid = requester_id(segment, bus, device, function);
        Some(match self.kind {
            1 => Target::Function(rid),
            2 => {
                let secondary = config.read8(segment, bus, device, function, SECONDARY_BUS);
                let subordinate = config.read8(segment, bus, device, function, SUBORDINATE_BUS);
                Target::Bridge(rid, secondary..=subordinate)
            }
            3 => Target::IoApic(self.enumeration_id, rid),
            _ => Target::Other,
        })
    }
}

impl Scope<'_> {
    /// The requester ID that the DMA of the function the scope names
    /// reaches the unit with when a PCI Express to PCI bridge on its path
    /// takes ownership of it: the secondary bus of the bridge nearest the
    /// unit, with device and function 0.
    pub fn alias(&self, segment: u16, config: &impl Config) -> Option<u64> {
        let (_, bridges) = self.path.as_chunks::<2>().0.split_last()?;
        let mut bus = self.start_bus;
        for &[device, function] in bridges {
            let secondary = config.read8(segment, bus, device, function, SECONDARY_BUS);
            if pcie_to_pci(config, segment, bus, device, function) {
                return Some(requester_id(segment, secondary, 0, 0));
            }
            bus = secondary;
        }
        None
    }
}

/// The device scope entries of a remapping structure, from its bytes
/// after the fixed part.
fn scopes(bytes: &[u8]) -> Vec<Scope<'_>> {
    let mut scopes = Vec::new();
    let mut at = 0;
    while let (Some(&kind), Some(&len)) = (bytes.get(at), bytes.get(at + 1)) {
        let len = usize::from(len);
        let Some(entry) = bytes.get(at..at + len).filter(|_| len >= 6) else {
            break;
        };
        scopes.push(Scope {
            kind,
            enumeration_id: entry[4],
            start_bus: entry[5],
            path: &entry[6..],
        });
        at += len;
    }
    scopes
}

/// A remapping structure the kernel uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Structure<'a> {
    /// A remapping unit (DRHD) at `base`, which covers the devices of its
    /// scopes or, with `include_all`, every device of its segment that no
    /// other unit names.
    Unit {
        segment: u16,
        base: u64,
        include_all: bool,
        scopes: Vec<Scope<'a>>,
    },
    /// Memory from `base` to `limit`, inclusive, reserved for the
    /// devices of its scopes (RMRR).
    Reserved {
        segment: u16,
        base: u64,
        limit: u64,
        scopes: Vec<Scope<'a>>,
    },
}

/// The remapping structures of `dmar` the kernel uses, in table order.
/// A structure that does not fit its length ends the list.
pub fn structures(dmar: &[u8]) -> Vec<Structure<'_>> {
    let mut out = Vec::new();
    let mut at = STRUCTURES;
    while let (Some(kind), Some(len)) = (u16_at(dmar, at), u16_at(dmar, at + 2)) {
        let len = usize::from(len);
        let Some(s) = dmar.get(at..at + len).filter(|_| len >= 4) else {
            break;
        };
        let parsed = match kind {
            0 => (|| {
                Some(Structure::Unit {
                    include_all: s.get(4)? & 1 != 0,
                    segment: u16_at(s, 6)?,
                    base: u64_at(s, 8)?,
                    scopes: scopes(s.get(16..)?),
                })
            })(),
            1 => (|| {
                Some(Structure::Reserved {
                    segment: u16_at(s, 6)?,
                    base: u64_at(s, 8)?,
                    limit: u64_at(s, 16)?,
                    scopes: scopes(s.get(24..)?),
                })
            })(),
            _ => None,
        };
        out.extend(parsed);
        at += len;
    }
    out
}

/// A remapping unit with its scopes resolved, and the requester IDs that
/// the DMA of named functions arrives with instead of their own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub segment: u16,
    pub base: u64,
    pub include_all: bool,
    pub targets: Vec<Target>,
    pub aliases: Vec<(u64, u64)>,
}

impl Unit {
    /// The requester ID the DMA of `rid` arrives with, when it is another
    /// one.
    pub fn alias(&self, rid: u64) -> Option<u64> {
        self.aliases
            .iter()
            .find(|&&(f, _)| f == rid)
            .map(|&(_, alias)| alias)
            .filter(|&alias| alias != rid)
    }

    /// Whether one of the unit's scopes names the function `rid`.
    fn names(&self, rid: u64) -> bool {
        let bus = (rid >> 8 & 0xFF) as u8;
        self.targets.iter().any(|t| match t {
            Target::Function(f) => *f == rid,
            Target::Bridge(b, buses) => *b == rid || buses.contains(&bus),
            _ => false,
        })
    }
}

/// The index of the unit in `units` that covers the function `rid`: the
/// first whose scopes name it, or else the one of its segment that
/// covers all other devices.
pub fn covering(units: &[Unit], rid: u64) -> Option<usize> {
    let segment = (rid >> 16) as u16;
    units
        .iter()
        .position(|u| u.segment == segment && u.names(rid))
        .or_else(|| {
            units
                .iter()
                .position(|u| u.segment == segment && u.include_all)
        })
}

#[cfg(test)]
mod tests;
