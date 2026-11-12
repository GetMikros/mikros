use super::*;
use boot_protocol::{Kind, VERSION_MINOR};
use std::vec::Vec;

fn sample() -> BootInfo {
    BootInfo {
        magic: MAGIC,
        version_major: VERSION_MAJOR,
        version_minor: VERSION_MINOR,
        size: BOOT_INFO_SIZE,
        arch: 1,
        flags: 0b101,
        memory_map_phys: 0x1000,
        memory_map_count: 3,
        memory_map_entry_size: 24,
        kernel_phys_base: 0x20_0000,
        kernel_virt_base: 0xffff_ffff_8000_0000,
        kernel_size: 0x5000,
        direct_map_base: 0xffff_8000_0000_0000,
        boot_image_phys: 0x40_0000,
        boot_image_size: 22,
        cmdline_phys: 0x2100,
        cmdline_len: 5,
        firmware_table_phys: 0x7fe_0000,
        uefi_system_table_phys: 0x7f0_0000,
        framebuffer: Framebuffer {
            phys_base: 0x8000_0000,
            size: 0x40_0000,
            width: 1280,
            height: 800,
            stride: 1280,
            format: 2,
            red_mask: 0xff_0000,
            green_mask: 0xff00,
            blue_mask: 0xff,
            reserved_mask: 0xff00_0000,
        },
        rng_seed: [7; 32],
        boot_time: 1_793_577_600,
        arch_data0: 0x9f000,
        arch_data1: 0,
        boot_time_ns: 1_793_577_600_000_000_000,
        framebuffer_pci_segment: 0,
        framebuffer_pci_bus: 3,
        framebuffer_pci_devfn: 2 * 8,
        framebuffer_pci_valid: 1,
        reserved: [0; 3],
    }
}

/// `info` without a framebuffer PCI function.
fn without_pci(mut info: BootInfo) -> BootInfo {
    info.framebuffer_pci_segment = 0;
    info.framebuffer_pci_bus = 0;
    info.framebuffer_pci_devfn = 0;
    info.framebuffer_pci_valid = 0;
    info
}

fn encode(info: &BootInfo) -> Vec<u8> {
    // SAFETY: BootInfo is repr(C) plain data without padding, so all of its
    // bytes are initialized.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (info as *const BootInfo).cast::<u8>(),
            size_of::<BootInfo>(),
        )
    };
    bytes.to_vec()
}

#[test]
fn decodes_every_field() {
    let info = sample();
    let data = encode(&info);
    assert_eq!(size(&data[..HEADER_SIZE]), Ok(0xF8));
    assert_eq!(decode(&data), Ok(info));
}

#[test]
fn reads_the_framebuffer_pci_function_only_from_version_1_3() {
    // Version 1.2, in its 0xF0 bytes.
    let mut info = sample();
    (info.version_minor, info.size) = (2, 0xF0);
    let mut data = encode(&info);
    data.truncate(0xF0);
    assert_eq!(decode(&data), Ok(without_pci(info)));
    // Version 1.3 with the function marked not valid.
    let mut info = sample();
    info.framebuffer_pci_valid = 0;
    assert_eq!(decode(&encode(&info)), Ok(without_pci(info)));
    // A version 1.2 structure of the new size still counts as without.
    let mut info = sample();
    info.version_minor = 2;
    assert_eq!(decode(&encode(&info)), Ok(without_pci(info)));
}

/// `sample` as version 1.0, in its 0xE8 bytes, with the flags `flags`.
fn version_1_0(flags: u32) -> (BootInfo, Vec<u8>) {
    let mut info = sample();
    (info.version_minor, info.size, info.flags) = (0, 0xE8, flags);
    let mut data = encode(&info);
    data.truncate(0xE8);
    (without_pci(info), data)
}

#[test]
fn derives_boot_time_ns_from_version_1_0() {
    let (info, data) = version_1_0(boot_protocol::flags::BOOT_TIME_VALID);
    assert_eq!(decode(&data), Ok(info));
    let (mut info, data) = version_1_0(0);
    info.boot_time_ns = 0;
    assert_eq!(decode(&data), Ok(info));
}

#[test]
fn reads_boot_time_ns_only_with_version_and_size() {
    // A version 1.1 structure the size of 1.0, and a 1.0 one with room
    // for the field, do not give it.
    let (mut info, mut data) = version_1_0(boot_protocol::flags::BOOT_TIME_VALID);
    data[0x0A] = 1;
    info.version_minor = 1;
    assert_eq!(decode(&data), Ok(info));
    let mut info = sample();
    info.version_minor = 0;
    let given = info.boot_time_ns;
    info.boot_time_ns = 7;
    let data = encode(&info);
    info.boot_time_ns = given;
    assert_eq!(decode(&data), Ok(without_pci(info)));
}

#[test]
fn derives_no_boot_time_ns_that_does_not_fit() {
    let (mut info, mut data) = version_1_0(boot_protocol::flags::BOOT_TIME_VALID);
    data[0xD0..0xD8].copy_from_slice(&i64::MAX.to_le_bytes());
    (info.flags, info.boot_time, info.boot_time_ns) = (0, 0, 0);
    assert_eq!(decode(&data), Ok(info));
}

#[test]
fn takes_mapped_runtime_services_only_from_version_1_2() {
    let mut info = sample();
    info.flags |= flags::RUNTIME_SERVICES_MAPPED;
    for (minor, mapped) in [(0, false), (1, false), (2, true), (3, true)] {
        info.version_minor = minor;
        let decoded = decode(&encode(&info)).unwrap();
        assert_eq!(
            decoded.flags & flags::RUNTIME_SERVICES_MAPPED != 0,
            mapped,
            "1.{minor}"
        );
        assert_eq!(decoded.flags & 0b111, info.flags & 0b111, "1.{minor}");
    }
}

#[test]
fn ignores_bytes_after_known_fields() {
    let mut info = sample();
    info.version_minor = 3;
    info.size = 0x100;
    let mut data = encode(&info);
    data.resize(0x100, 0xaa);
    assert_eq!(decode(&data), Ok(info));
}

#[test]
fn refuses_bad_headers() {
    let mut data = encode(&sample());
    data[0] = b'X';
    assert_eq!(decode(&data), Err(Error::BadMagic));

    let mut info = sample();
    info.version_major = 2;
    assert_eq!(
        decode(&encode(&info)),
        Err(Error::UnsupportedVersion(2, VERSION_MINOR))
    );

    info = sample();
    info.size = 0xE0;
    assert_eq!(decode(&encode(&info)), Err(Error::BadSize(0xE0)));

    info = sample();
    info.size = 0x100;
    assert_eq!(decode(&encode(&info)), Err(Error::Truncated));
    assert_eq!(size(&[0; 4]), Err(Error::Truncated));
}

#[test]
fn decodes_memory_map_entries() {
    let mut data = Vec::new();
    for (start, pages, kind) in [
        (0u64, 159u64, Kind::Usable),
        (0x10_0000, 16, Kind::KernelImage),
    ] {
        data.extend_from_slice(&start.to_le_bytes());
        data.extend_from_slice(&pages.to_le_bytes());
        data.extend_from_slice(&(kind as u32).to_le_bytes());
        data.extend_from_slice(&[0; 4]);
        data.extend_from_slice(&[0xee; 8]);
    }
    assert_eq!(memory_map_size(2, 32), Ok(Some(64)));
    let entries: Vec<_> = memory_map(&data, 32).unwrap().collect();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].phys_start, 0x10_0000);
    assert_eq!(entries[1].page_count, 16);
    assert_eq!(Kind::from_u32(entries[1].kind), Some(Kind::KernelImage));
    assert!(memory_map(&data, 16).is_err());
    assert_eq!(memory_map_size(1, 8), Err(Error::BadEntrySize(8)));
    assert_eq!(memory_map_size(u64::MAX, 24), Ok(None));
}

fn entry(phys_start: u64, page_count: u64, kind: Kind) -> MemoryMapEntry {
    MemoryMapEntry {
        phys_start,
        page_count,
        kind: kind as u32,
        reserved: 0,
    }
}

#[test]
fn checks_framebuffer_geometry() {
    // 800 by 600 pixels with 32 pixels of padding per line, in two
    // Framebuffer ranges listed out of order.
    let fb = Framebuffer {
        phys_base: 0x8000_0000,
        size: 832 * 600 * 4,
        width: 800,
        height: 600,
        stride: 832,
        ..Framebuffer::default()
    };
    let pages = fb.size.div_ceil(4096);
    let map = [
        entry(0x8000_0000 + 4096 * 100, pages - 100, Kind::Framebuffer),
        entry(0, 16, Kind::Usable),
        entry(0x8000_0000, 100, Kind::Framebuffer),
    ];
    let check =
        |fb: &Framebuffer, map: &[MemoryMapEntry]| check_framebuffer(fb, map.iter().copied());
    assert_eq!(check(&fb, &map), Ok(()));
    let with = |change: fn(&mut Framebuffer)| {
        let mut f = fb;
        change(&mut f);
        check(&f, &map)
    };
    assert_eq!(with(|f| f.width = 0), Err(FramebufferError::Empty));
    assert_eq!(with(|f| f.height = 0), Err(FramebufferError::Empty));
    assert_eq!(
        with(|f| f.stride = 799),
        Err(FramebufferError::NarrowStride)
    );
    assert_eq!(with(|f| f.size -= 1), Err(FramebufferError::TooSmall));
    assert_eq!(
        with(|f| (f.stride, f.height) = (u32::MAX, u32::MAX)),
        Err(FramebufferError::TooSmall)
    );
    assert_eq!(
        with(|f| f.phys_base = u64::MAX - 4096),
        Err(FramebufferError::RangeOverflow)
    );
    // A range of another kind, or a gap, does not cover it.
    let mut other = map;
    other[2].kind = Kind::Mmio as u32;
    assert_eq!(check(&fb, &other), Err(FramebufferError::NotInMemoryMap));
    assert_eq!(check(&fb, &map[..2]), Err(FramebufferError::NotInMemoryMap));
    assert_eq!(check(&fb, &[]), Err(FramebufferError::NotInMemoryMap));
}
