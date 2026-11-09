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
        (5, Kind::FirmwareRuntimeCode),
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
        assert_eq!(firmware_kind(uefi_type, 0), kind, "type {uefi_type}");
    }
}

#[test]
fn gives_every_runtime_range_a_runtime_kind() {
    let expected = [
        (5, Kind::FirmwareRuntimeCode),
        (6, Kind::FirmwareRuntime),
        (11, Kind::FirmwareRuntimeMmio),
        (12, Kind::FirmwareRuntimeMmio),
        // Types that give other kinds without the attribute.
        (0, Kind::FirmwareRuntime),
        (7, Kind::FirmwareRuntime),
        (10, Kind::FirmwareRuntime),
        (14, Kind::FirmwareRuntime),
    ];
    for (uefi_type, kind) in expected {
        let attribute = EFI_MEMORY_RUNTIME | 0xF;
        assert_eq!(
            firmware_kind(uefi_type, attribute),
            kind,
            "type {uefi_type}"
        );
    }
    // Other attributes change nothing.
    assert_eq!(firmware_kind(11, 1 << 62), Kind::Mmio);
}

#[test]
fn sorts_and_rejects_overlapping_firmware_ranges() {
    let mut buf = [MemoryMapEntry::default(); 8];
    let mut map = Map::new(&mut buf);
    map.push_firmware(10 * P, 2, 7, 0).unwrap();
    map.push_firmware(0, 4, 7, 0).unwrap();
    map.push_firmware(4 * P, 0, 7, 0).unwrap();
    map.sort().unwrap();
    assert_eq!(list(&map), [(0, 4, Kind::Usable), (10, 2, Kind::Usable)]);
    map.push_firmware(11 * P, 1, 7, 0).unwrap();
    assert_eq!(map.sort(), Err(Error::Overlap(11 * P)));
}

#[test]
fn overlays_split_ranges_and_fill_gaps() {
    let mut buf = [MemoryMapEntry::default(); 16];
    let mut map = Map::new(&mut buf);
    map.push_firmware(0, 10, 2, 0).unwrap();
    map.push_firmware(20 * P, 5, 7, 0).unwrap();
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
        map.push_firmware(start * P, pages, t, 0).unwrap();
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
    map.push_firmware(0, 10, 7, 0).unwrap();
    map.sort().unwrap();
    map.overlay(Region {
        start: 2 * P,
        pages: 1,
        kind: Kind::KernelStack,
    })
    .unwrap_err();
}

#[test]
fn ends_the_direct_map_at_its_highest_covered_range() {
    let mut buf = [MemoryMapEntry::default(); 4];
    let mut map = Map::new(&mut buf);
    assert_eq!(map.direct_map_end(), 0);
    map.push_firmware(0x10_0000, 4, 7, 0).unwrap();
    // MMIO above conventional memory is not in the direct map.
    map.push_firmware(0xfe00_0000, 1, 11, 0).unwrap();
    map.sort().unwrap();
    assert_eq!(map.direct_map_end(), 0x10_0000 + 4 * P);
}

#[test]
fn checks_the_direct_map_limit() {
    let mut buf = [MemoryMapEntry::default(); 4];
    let mut map = Map::new(&mut buf);
    let limit = 64 << 40;
    map.push_firmware(limit - P, 1, 7, 0).unwrap();
    map.push_firmware(limit, 1, 11, 0).unwrap();
    map.sort().unwrap();
    assert_eq!(
        map.check_direct_map_limit(limit),
        Ok(()),
        "MMIO above is fine"
    );
    map.push_firmware(limit + P, 1, 9, 0).unwrap();
    map.sort().unwrap();
    assert_eq!(
        map.check_direct_map_limit(limit),
        Err(Error::AboveDirectMapLimit(limit + 2 * P))
    );
}
