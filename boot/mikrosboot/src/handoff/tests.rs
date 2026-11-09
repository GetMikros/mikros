use super::*;
use crate::paging::direct_runs;
use alloc::vec;
use alloc::vec::Vec;
use boot_protocol::x86_64::DIRECT_MAP_BASE;
use boot_protocol::{CMDLINE_MAX, flags};

const P: u64 = PAGE_SIZE;
const CONVENTIONAL: u32 = 7;
const LOADER_DATA: u32 = 2;
const INFO: u64 = 0x32_0000;
const ENTRIES: u64 = 0x32_2000;

fn prepared() -> Prepared {
    Prepared {
        kernel: Kernel {
            entry: 0xffff_ffff_8000_0000,
            segments: Vec::new(),
            version: (VERSION_MAJOR, VERSION_MINOR),
            relocatable: false,
            relocations: Vec::new(),
        },
        layout: Layout {
            virt_base: 0xffff_ffff_8000_0000,
            size: 16 * P,
        },
        kernel_phys: 0x20_0000,
        boot_image_phys: 0x30_0000,
        boot_image_size: 3 * P - 5,
        stack_phys: 0x31_0000,
        trampoline: Some(0x8000),
        info_phys: INFO,
        cmdline_len: 12,
        entries_phys: ENTRIES,
        entries_capacity: 64,
        firmware_map_phys: 0x32_3000,
        firmware_map_size: 2 * P as usize,
        tables_phys: 0x33_0000,
        tables: 8,
        framebuffer: Some(Framebuffer {
            phys_base: 0xfd00_0000,
            size: 0x30_0000,
            width: 1024,
            ..Framebuffer::default()
        }),
        rsdp: 0x7fe_0014,
        system_table: 0x7f0_0000,
        rng_seed: None,
        boot_time: None,
        direct_map_base: DIRECT_MAP_BASE,
    }
}

/// A firmware map in which the bootloader's allocations are loader data.
fn firmware() -> [(u64, u64, u32); 5] {
    [
        (0, 8, CONVENTIONAL),
        (0x8000, 1, LOADER_DATA),
        (0x9000, 0x97, CONVENTIONAL),
        (0x10_0000, 0x100, CONVENTIONAL),
        (0x20_0000, 0x200, LOADER_DATA),
    ]
}

fn kind_at(entries: &[MemoryMapEntry], phys: u64) -> Option<Kind> {
    entries
        .iter()
        .find(|e| e.phys_start <= phys && phys < e.phys_start + e.page_count * P)
        .and_then(|e| Kind::from_u32(e.kind))
}

fn direct_mapped(entries: &[MemoryMapEntry], start: u64, end: u64) -> bool {
    direct_runs(entries).any(|(s, e)| s <= start && end <= e)
}

#[test]
fn gives_allocations_the_kind_of_their_contents() {
    let p = prepared();
    let mut buf = vec![MemoryMapEntry::default(); p.entries_capacity];
    let mut map = Map::new(&mut buf);
    p.memory_map(&mut map, firmware()).unwrap();
    let entries = map.entries();
    let expected = [
        (0x8000, Kind::ApTrampoline),
        (0x20_0000, Kind::KernelImage),
        (0x20_f000, Kind::KernelImage),
        (0x21_0000, Kind::BootloaderReclaimable),
        (0x30_2000, Kind::BootImage),
        (0x31_f000, Kind::KernelStack),
        (0x33_7000, Kind::PageTables),
        (0xfd2f_f000, Kind::Framebuffer),
    ];
    for (phys, kind) in expected {
        assert_eq!(kind_at(entries, phys), Some(kind), "{phys:#x}");
    }
}

#[test]
fn keeps_handoff_data_reclaimable_and_direct_mapped() {
    let p = prepared();
    let mut buf = vec![MemoryMapEntry::default(); p.entries_capacity];
    let mut map = Map::new(&mut buf);
    p.memory_map(&mut map, firmware()).unwrap();
    let entries = map.entries();
    let cmdline = INFO + CMDLINE_OFFSET;
    let handoff = [
        (INFO, INFO + size_of::<BootInfo>() as u64),
        (cmdline, cmdline + CMDLINE_MAX as u64),
        (
            ENTRIES,
            ENTRIES + (p.entries_capacity * size_of::<MemoryMapEntry>()) as u64,
        ),
    ];
    for (start, end) in handoff {
        for page in (start..end).step_by(P as usize) {
            assert_eq!(
                kind_at(entries, page),
                Some(Kind::BootloaderReclaimable),
                "{page:#x}"
            );
        }
        assert!(direct_mapped(entries, start, end), "{start:#x}");
    }
}

#[test]
fn fits_boot_info_and_the_command_line_in_their_pages() {
    assert!(u64::from(BOOT_INFO_SIZE) <= CMDLINE_OFFSET);
    assert!(CMDLINE_OFFSET + CMDLINE_MAX as u64 <= INFO_PAGES * P);
}

#[test]
fn refuses_a_map_the_direct_map_cannot_cover() {
    let p = prepared();
    let mut buf = vec![MemoryMapEntry::default(); p.entries_capacity];
    let mut map = Map::new(&mut buf);
    let mut firmware = firmware().to_vec();
    firmware.push((DIRECT_MAP_LIMIT - P, 2, LOADER_DATA));
    assert_eq!(
        p.memory_map(&mut map, firmware),
        Err(memmap::Error::AboveDirectMapLimit(DIRECT_MAP_LIMIT + P))
    );
}

#[test]
fn fills_boot_info_with_physical_addresses() {
    let p = prepared();
    let entries = [MemoryMapEntry::default(); 3];
    let info = p.boot_info(&entries);
    assert_eq!((info.magic, info.size), (MAGIC, BOOT_INFO_SIZE));
    assert_eq!(info.memory_map_phys, entries.as_ptr() as u64);
    assert_eq!(info.memory_map_count, 3);
    assert_eq!(info.cmdline_phys, INFO + CMDLINE_OFFSET);
    assert_eq!(info.cmdline_len, 12);
    assert_eq!(info.uefi_system_table_phys, 0x7f0_0000);
    assert_eq!(info.firmware_table_phys, 0x7fe_0014);
    assert_eq!(info.arch_data0, 0x8000);
    assert_eq!(info.framebuffer.phys_base, 0xfd00_0000);
    assert_eq!(info.flags, flags::FRAMEBUFFER_PRESENT);
    assert_eq!(
        (info.kernel_phys_base, info.kernel_size),
        (0x20_0000, 16 * P)
    );
    assert_eq!(
        (info.boot_image_phys, info.boot_image_size),
        (0x30_0000, 3 * P - 5)
    );
    assert_eq!(
        (info.kernel_virt_base, info.direct_map_base),
        (0xffff_ffff_8000_0000, DIRECT_MAP_BASE)
    );
}

#[test]
fn gives_zero_addresses_for_what_is_absent() {
    let p = Prepared {
        cmdline_len: 0,
        trampoline: None,
        framebuffer: None,
        ..prepared()
    };
    let info = p.boot_info(&[]);
    assert_eq!(info.cmdline_phys, 0);
    assert_eq!(info.arch_data0, 0);
    assert_eq!(info.framebuffer, Framebuffer::default());
    assert_eq!(info.flags, 0);
    assert_eq!(info.arch_data1, 0);
    let mut buf = vec![MemoryMapEntry::default(); p.entries_capacity];
    let mut map = Map::new(&mut buf);
    p.memory_map(&mut map, firmware()).unwrap();
    assert_eq!(
        kind_at(map.entries(), 0x8000),
        Some(Kind::BootloaderReclaimable)
    );
    assert_eq!(kind_at(map.entries(), 0xfd00_0000), None);
}
