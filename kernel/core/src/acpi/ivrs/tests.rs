use super::*;
use std::vec;
use std::vec::Vec;

fn ivhd(kind: u8, device: u16, base: u64, entries: &[&[u8]]) -> Vec<u8> {
    let mut b = vec![kind, 0, 0, 0];
    b.extend_from_slice(&device.to_le_bytes());
    b.extend_from_slice(&0x40u16.to_le_bytes());
    b.extend_from_slice(&base.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.resize(if kind == 0x10 { 24 } else { 40 }, 0);
    entries.iter().for_each(|e| b.extend_from_slice(e));
    let len = b.len() as u16;
    b[2..4].copy_from_slice(&len.to_le_bytes());
    b
}

fn ivmd(kind: u8, flags: u8, first: u16, aux: u16, base: u64, length: u64) -> Vec<u8> {
    let mut b = vec![kind, flags, 32, 0];
    b.extend_from_slice(&first.to_le_bytes());
    b.extend_from_slice(&aux.to_le_bytes());
    b.extend_from_slice(&[0; 8]);
    b.extend_from_slice(&base.to_le_bytes());
    b.extend_from_slice(&length.to_le_bytes());
    b
}

fn ivrs(blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut t = vec![0; HEADER_SIZE + 12];
    t[..4].copy_from_slice(b"IVRS");
    blocks.iter().for_each(|b| t.extend_from_slice(b));
    t
}

fn entry4(kind: u8, id: u16) -> Vec<u8> {
    let mut e = vec![kind];
    e.extend_from_slice(&id.to_le_bytes());
    e.push(0);
    e
}

#[test]
fn reads_device_entries() {
    let special = [72, 0, 0, 0, 9, 0xA0, 0, 1];
    let alias = [66, 0x08, 0, 0, 0, 0x10, 0, 0];
    let hid = {
        let mut e = vec![0xF0; 22];
        e[21] = 3;
        e.extend_from_slice(b"uid");
        e
    };
    let table = ivrs(&[ivhd(
        0x10,
        0x02,
        0xFEB8_0000,
        &[
            &entry4(3, 0x100),
            &entry4(0, 0),
            &entry4(4, 0x1FF),
            &entry4(2, 0x08),
            &alias,
            &hid,
            &special,
        ],
    )]);
    let units = units(&table);
    assert_eq!(units.len(), 1);
    let u = &units[0];
    assert_eq!(
        (u.kind, u.device, u.capability, u.base),
        (0x10, 2, 0x40, 0xFEB8_0000)
    );
    assert_eq!(
        u.devices,
        [0x100..=0x1FF, 0x08..=0x08, 0x08..=0x08, 0xA0..=0xA0]
    );
    assert_eq!(u.io_apics, [(9, 0xA0)]);
    assert_eq!(u.aliases, [(0x08..=0x08, 0x10)]);
    assert_eq!((u.alias(0x08), u.alias(0x09)), (Some(0x10), None));
    assert!(u.covers(0x150) && u.covers(0xA0) && !u.covers(0x200));
    assert!(!u.covers(0x1_0150), "another segment");
}

/// Each device entry's DTE setting covers the IDs it names: a range from
/// its start entry, both IDs of an alias, and a special device's ID. The
/// last entry that covers an ID decides, and an ID that none covers gets
/// 0.
#[test]
fn reads_dte_settings() {
    let with = |mut e: Vec<u8>, setting: u8| {
        e[3] = setting;
        e
    };
    let alias = [66, 0x40, 0, 0x04, 0, 0x41, 0, 0];
    let special = [72, 0, 0, 0xD7, 9, 0xA0, 0, 1];
    let table = ivrs(&[ivhd(
        0x10,
        0x02,
        0xFEB8_0000,
        &[
            &with(entry4(1, 0), 0x01),
            &with(entry4(2, 0x10), 0xC7),
            &with(entry4(3, 0x20), 0x30),
            &entry4(4, 0x2F),
            &alias,
            &special,
        ],
    )]);
    let u = &units(&table)[0];
    assert_eq!(u.setting(0x10), 0xC7);
    assert_eq!((u.setting(0x20), u.setting(0x2F)), (0x30, 0x30));
    assert_eq!((u.setting(0x40), u.setting(0x41)), (0x04, 0x04));
    assert_eq!(u.setting(0xA0), 0xD7);
    assert_eq!(u.setting(0x30), 0x01, "the entry for all devices");
    let table = ivrs(&[ivhd(0x10, 0x02, 0x1000, &[&with(entry4(2, 0x10), 0xC7)])]);
    assert_eq!(units(&table)[0].setting(0x11), 0);
}

#[test]
fn places_dte_settings_in_the_device_table_entry() {
    assert_eq!(dte_bits(0), [0, 0]);
    // INITPass, EIntPass and NMIPass.
    assert_eq!(dte_bits(0b111), [0, 0b111 << 56]);
    // SysMgt.
    assert_eq!(dte_bits(0b10 << 4), [0b10 << 40, 0]);
    // Lint0Pass and Lint1Pass.
    assert_eq!(dte_bits(0b11 << 6), [0, 0b11 << 62]);
    // Bit 3 is reserved.
    assert_eq!(dte_bits(1 << 3), [0, 0]);
}

#[test]
fn prefers_the_block_of_the_highest_type() {
    let all = entry4(1, 0);
    let table = ivrs(&[
        ivhd(0x10, 0x02, 0x1000, &[&entry4(2, 5)]),
        ivhd(0x11, 0x04, 0x1000, &[&all]),
        ivhd(0x10, 0x02, 0x2000, &[]),
    ]);
    let units = units(&table);
    assert_eq!(units.len(), 2);
    assert_eq!(units[0].kind, 0x11);
    assert_eq!(units[0].devices, vec![0..=u16::MAX]);
    assert_eq!(units[1].base, 0x2000, "another unit of the same device ID");
}

#[test]
fn reads_alias_ranges() {
    let start = [67, 0x20, 0, 0, 0, 0x18, 0, 0];
    let table = ivrs(&[ivhd(0x10, 0x02, 0x1000, &[&start, &entry4(4, 0x2F)])]);
    let u = &units(&table)[0];
    assert_eq!(u.devices, vec![0x20..=0x2F]);
    assert_eq!(u.alias(0x25), Some(0x18));
}

#[test]
fn reads_reserved_memory() {
    let table = ivrs(&[
        ivmd(0x20, 1 << 3, 0, 0, 0xA000_0000, 0x1000),
        ivmd(0x21, 1 << 1, 0x18, 0, 0xB000_0000, 0x2000),
        ivmd(0x22, 1 << 1 | 1 << 2, 0x20, 0x2F, 0xC000_0000, 0x3000),
        ivhd(0x10, 0x02, 0x1000, &[]),
    ]);
    let r = reserved(&table);
    assert_eq!(r.len(), 3);
    assert_eq!(
        (r[0].devices.clone(), r[0].read, r[0].write),
        (Devices::All, false, false),
        "an exclusion range gives only what its flags allow"
    );
    assert_eq!(
        (r[1].devices.clone(), r[1].read, r[1].write),
        (Devices::Range(0x18..=0x18), true, false)
    );
    assert_eq!(r[2].devices, Devices::Range(0x20..=0x2F));
    assert_eq!((r[2].base, r[2].length), (0xC000_0000, 0x3000));
}

#[test]
fn stops_at_a_block_that_does_not_fit() {
    let mut table = ivrs(&[ivhd(0x10, 0x02, 0x1000, &[])]);
    table.extend_from_slice(&[0x10, 0, 200, 0]);
    assert_eq!(units(&table).len(), 1);
    let mut cut = ivrs(&[ivhd(0x10, 0x02, 0x1000, &[&entry4(3, 1)])]);
    cut.extend_from_slice(&[0x10, 0, 2, 0]);
    assert_eq!(units(&cut)[0].devices, [], "a range that is never closed");
}
