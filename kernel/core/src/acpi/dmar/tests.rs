use super::*;
use std::vec;
use std::vec::Vec;

fn scope(kind: u8, id: u8, bus: u8, path: &[u8]) -> Vec<u8> {
    let mut s = vec![kind, 6 + path.len() as u8, 0, 0, id, bus];
    s.extend_from_slice(path);
    s
}

fn unit(include_all: bool, segment: u16, base: u64, scopes: &[Vec<u8>]) -> Vec<u8> {
    let mut s = vec![0, 0, 0, 0, u8::from(include_all), 0];
    s.extend_from_slice(&segment.to_le_bytes());
    s.extend_from_slice(&base.to_le_bytes());
    scopes.iter().for_each(|sc| s.extend_from_slice(sc));
    let len = s.len() as u16;
    s[2..4].copy_from_slice(&len.to_le_bytes());
    s
}

fn reserved(segment: u16, base: u64, limit: u64, scopes: &[Vec<u8>]) -> Vec<u8> {
    let mut s = vec![1, 0, 0, 0, 0, 0];
    s.extend_from_slice(&segment.to_le_bytes());
    s.extend_from_slice(&base.to_le_bytes());
    s.extend_from_slice(&limit.to_le_bytes());
    scopes.iter().for_each(|sc| s.extend_from_slice(sc));
    let len = s.len() as u16;
    s[2..4].copy_from_slice(&len.to_le_bytes());
    s
}

fn dmar(flags: u8, structures: &[Vec<u8>]) -> Vec<u8> {
    let mut t = vec![0; HEADER_SIZE];
    t[..4].copy_from_slice(b"DMAR");
    t.extend_from_slice(&[38, flags]);
    t.extend_from_slice(&[0; 10]);
    structures.iter().for_each(|s| t.extend_from_slice(s));
    t
}

/// A PCI Express root port at 0:1c.0 whose secondary bus is 3 and
/// subordinate bus 5, and below it a PCI Express to PCI bridge at 3:0.0
/// whose secondary bus is 4.
struct Bridges;

impl Config for Bridges {
    fn read8(&self, segment: u16, bus: u8, device: u8, function: u8, offset: u16) -> u8 {
        match (segment, bus, device, function, offset) {
            (0, 0, 0x1C, 0, SECONDARY_BUS) => 3,
            (0, 0, 0x1C, 0, SUBORDINATE_BUS) => 5,
            (0, 3, 0, 0, SECONDARY_BUS) => 4,
            (0, 0, 0x1C, 0, STATUS) | (0, 3, 0, 0, STATUS) => 1 << 4,
            (0, 0, 0x1C, 0, CAPABILITIES) | (0, 3, 0, 0, CAPABILITIES) => 0x40,
            // A power management capability, then PCI Express.
            (0, _, _, 0, 0x40) => 0x01,
            (0, _, _, 0, 0x41) => 0x50,
            (0, _, _, 0, 0x50) => PCI_EXPRESS,
            (0, _, _, 0, 0x51) => 0,
            // A root port, and a PCI Express to PCI bridge.
            (0, 0, 0x1C, 0, 0x52) => 0x4 << 4,
            (0, 3, 0, 0, 0x52) => PCIE_TO_PCI << 4,
            _ => 0xFF,
        }
    }
}

#[test]
fn builds_requester_ids() {
    assert_eq!(requester_id(0, 0, 0x1F, 3), 0xFB);
    assert_eq!(requester_id(1, 2, 3, 4), 0x1_021C);
}

#[test]
fn reads_units_and_reserved_memory() {
    let gpu = scope(1, 0, 0, &[2, 0]);
    let table = dmar(
        INTR_REMAP | DMA_CTRL_PLATFORM_OPT_IN,
        &[
            unit(false, 0, 0xFED9_0000, std::slice::from_ref(&gpu)),
            unit(true, 0, 0xFED9_1000, &[scope(3, 2, 0xF0, &[0x1F, 0])]),
            vec![2, 0, 8, 0, 0, 0, 0, 0],
            reserved(0, 0x7A00_0000, 0x7A3F_FFFF, &[gpu]),
        ],
    );
    assert_eq!(flags(&table), INTR_REMAP | DMA_CTRL_PLATFORM_OPT_IN);
    let s = structures(&table);
    assert_eq!(s.len(), 3, "the ATSR is skipped");
    let Structure::Unit {
        include_all,
        base,
        scopes,
        ..
    } = &s[1]
    else {
        panic!("a unit");
    };
    assert!(*include_all);
    assert_eq!(*base, 0xFED9_1000);
    assert_eq!(
        scopes[0].resolve(0, &Bridges),
        Some(Target::IoApic(2, 0xF0F8))
    );
    assert_eq!(
        s[2],
        Structure::Reserved {
            segment: 0,
            base: 0x7A00_0000,
            limit: 0x7A3F_FFFF,
            scopes: vec![Scope {
                kind: 1,
                enumeration_id: 0,
                start_bus: 0,
                path: &[2, 0],
            }],
        }
    );
}

#[test]
fn stops_at_a_structure_that_does_not_fit() {
    let mut table = dmar(0, &[unit(true, 0, 0x1000, &[])]);
    table.extend_from_slice(&[0, 0, 200, 0]);
    assert_eq!(structures(&table).len(), 1);
    let mut short = dmar(0, &[]);
    short.extend_from_slice(&[0, 0, 2, 0]);
    assert!(structures(&short).is_empty());
}

#[test]
fn follows_paths_through_bridges() {
    let behind = Scope {
        kind: 1,
        enumeration_id: 0,
        start_bus: 0,
        path: &[0x1C, 0, 0, 1],
    };
    assert_eq!(behind.resolve(0, &Bridges), Some(Target::Function(0x301)));
    assert_eq!(behind.alias(0, &Bridges), None, "behind a root port only");
    let conventional = Scope {
        path: &[0x1C, 0, 0, 0, 5, 0],
        ..behind
    };
    assert_eq!(
        conventional.resolve(0, &Bridges),
        Some(Target::Function(0x428))
    );
    assert_eq!(
        conventional.alias(0, &Bridges),
        Some(0x400),
        "the bridge owns it"
    );
    let bridge = Scope {
        kind: 2,
        enumeration_id: 0,
        start_bus: 0,
        path: &[0x1C, 0],
    };
    assert_eq!(
        bridge.resolve(0, &Bridges),
        Some(Target::Bridge(0xE0, 3..=5))
    );
    let empty = Scope {
        path: &[],
        ..bridge
    };
    assert_eq!(empty.resolve(0, &Bridges), None);
}

#[test]
fn finds_the_covering_unit() {
    let units = [
        Unit {
            segment: 0,
            base: 0x1000,
            include_all: false,
            targets: vec![Target::Function(0x10), Target::Bridge(0xE0, 3..=5)],
            aliases: vec![(0x10, 0x200), (0x18, 0x18)],
        },
        Unit {
            segment: 0,
            base: 0x2000,
            include_all: true,
            targets: vec![],
            aliases: vec![],
        },
        Unit {
            segment: 1,
            base: 0x3000,
            include_all: false,
            targets: vec![Target::Function(0x1_0008)],
            aliases: vec![],
        },
    ];
    assert_eq!(units[0].alias(0x10), Some(0x200));
    assert_eq!(units[0].alias(0x18), None, "an alias of itself");
    assert_eq!(covering(&units, 0x10), Some(0));
    assert_eq!(covering(&units, 0x400), Some(0), "below the bridge");
    assert_eq!(covering(&units, 0xE0), Some(0), "the bridge itself");
    assert_eq!(covering(&units, 0x18), Some(1), "everything else");
    assert_eq!(covering(&units, 0x1_0008), Some(2));
    assert_eq!(covering(&units, 0x1_0010), None, "no unit covers it");
}
