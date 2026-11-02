use super::*;

const P: u64 = PAGE_SIZE;

fn list(map: &Map) -> Vec<(u64, u64, Kind)> {
    map.entries()
        .iter()
        .map(|e| {
            (
                e.phys_start / P,
                e.page_count,
                Kind::from_u32(e.kind).unwrap(),
            )
        })
        .collect()
}

#[test]
fn translates_firmware_types() {
    let expected = [
        (0, Kind::Reserved),
        (1, Kind::BootloaderReclaimable),
        (2, Kind::BootloaderReclaimable),
        (3, Kind::Usable),
        (4, Kind::Usable),
        (5, Kind::FirmwareRuntime),
        (6, Kind::FirmwareRuntime),
        (7, Kind::Usable),
        (8, Kind::Bad),
        (9, Kind::AcpiReclaimable),
        (10, Kind::AcpiNvs),
        (11, Kind::Mmio),
        (12, Kind::Mmio),
        (13, Kind::Reserved),
        (14, Kind::Reserved),
    ];
    for (uefi_type, kind) in expected {
        assert_eq!(firmware_kind(uefi_type), kind, "type {uefi_type}");
    }
}

#[test]
fn sorts_and_rejects_overlapping_firmware_ranges() {
    let mut buf = [MemoryMapEntry::default(); 8];
    let mut map = Map::new(&mut buf);
    map.push_firmware(10 * P, 2, 7).unwrap();
    map.push_firmware(0, 4, 7).unwrap();
    map.push_firmware(4 * P, 0, 7).unwrap();
    map.sort().unwrap();
    assert_eq!(list(&map), [(0, 4, Kind::Usable), (10, 2, Kind::Usable)]);
    map.push_firmware(11 * P, 1, 7).unwrap();
    assert_eq!(map.sort(), Err(Error::Overlap(11 * P)));
}

#[test]
fn overlays_split_ranges_and_fill_gaps() {
    let mut buf = [MemoryMapEntry::default(); 16];
    let mut map = Map::new(&mut buf);
    map.push_firmware(0, 10, 2).unwrap();
    map.push_firmware(20 * P, 5, 7).unwrap();
    map.sort().unwrap();
    map.overlay(Region {
        start: 3 * P,
        pages: 2,
        kind: Kind::KernelImage,
    })
    .unwrap();
    map.overlay(Region {
        start: 8 * P,
        pages: 14,
        kind: Kind::BootImage,
    })
    .unwrap();
    map.overlay(Region::covering(40 * P + 16, P, Kind::Framebuffer))
        .unwrap();
    assert_eq!(
        list(&map),
        [
            (0, 3, Kind::BootloaderReclaimable),
            (3, 2, Kind::KernelImage),
            (5, 3, Kind::BootloaderReclaimable),
            (8, 14, Kind::BootImage),
            (22, 3, Kind::Usable),
            (40, 2, Kind::Framebuffer),
        ]
    );
}

#[test]
fn merges_adjacent_ranges_of_one_kind() {
    let mut buf = [MemoryMapEntry::default(); 8];
    let mut map = Map::new(&mut buf);
    for (start, pages, t) in [(0, 2, 7), (2, 3, 3), (5, 1, 2), (7, 1, 7), (8, 1, 7)] {
        map.push_firmware(start * P, pages, t).unwrap();
    }
    map.sort().unwrap();
    map.merge();
    assert_eq!(
        list(&map),
        [
            (0, 5, Kind::Usable),
            (5, 1, Kind::BootloaderReclaimable),
            (7, 2, Kind::Usable)
        ]
    );
}

#[test]
fn reports_a_full_buffer() {
    let mut buf = [MemoryMapEntry::default(); 2];
    let mut map = Map::new(&mut buf);
    map.push_firmware(0, 10, 7).unwrap();
    map.sort().unwrap();
    map.overlay(Region {
        start: 2 * P,
        pages: 1,
        kind: Kind::KernelStack,
    })
    .unwrap_err();
}

#[test]
fn checks_the_direct_map_limit() {
    let mut buf = [MemoryMapEntry::default(); 4];
    let mut map = Map::new(&mut buf);
    let limit = 64 << 40;
    map.push_firmware(limit - P, 1, 7).unwrap();
    map.push_firmware(limit, 1, 11).unwrap();
    map.sort().unwrap();
    assert_eq!(
        map.check_direct_map_limit(limit),
        Ok(()),
        "MMIO above is fine"
    );
    map.push_firmware(limit + P, 1, 9).unwrap();
    map.sort().unwrap();
    assert_eq!(
        map.check_direct_map_limit(limit),
        Err(Error::AboveDirectMapLimit(limit + 2 * P))
    );
}
