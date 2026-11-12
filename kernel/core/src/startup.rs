//! The bytes of the startup message the initial process receives.

use alloc::vec::Vec;
use boot_protocol::{BootInfo, MemoryMapEntry, flags};

/// The size of the message header.
pub const HEADER_SIZE: usize = 0x68;
/// The size of an encoded memory map entry.
pub const ENTRY_SIZE: usize = 24;

/// Header flag: a framebuffer is present.
pub const FRAMEBUFFER_PRESENT: u32 = 1 << 0;
/// Header flag: `boot_time` is valid.
pub const BOOT_TIME_VALID: u32 = 1 << 1;
/// Header flag: `framebuffer_pci` is valid.
pub const FRAMEBUFFER_PCI_VALID: u32 = 1 << 2;

/// The startup message for `info`, with the command line `cmdline` and
/// the entries of `memory_map`.
pub fn message(
    info: &BootInfo,
    cmdline: &[u8],
    memory_map: impl Iterator<Item = MemoryMapEntry>,
) -> Vec<u8> {
    let mut m = Vec::with_capacity(HEADER_SIZE + cmdline.len());
    m.resize(HEADER_SIZE, 0);
    let mut header_flags = 0;
    if info.flags & flags::FRAMEBUFFER_PRESENT != 0 {
        header_flags |= FRAMEBUFFER_PRESENT;
    }
    if info.flags & flags::BOOT_TIME_VALID != 0 {
        header_flags |= BOOT_TIME_VALID;
    }
    // The framebuffer's PCI function: segment, bus and devfn in one word.
    let mut framebuffer_pci = 0;
    if info.flags & flags::FRAMEBUFFER_PRESENT != 0 && info.framebuffer_pci_valid == 1 {
        header_flags |= FRAMEBUFFER_PCI_VALID;
        framebuffer_pci = (u32::from(info.framebuffer_pci_segment) << 16)
            | (u32::from(info.framebuffer_pci_bus) << 8)
            | u32::from(info.framebuffer_pci_devfn);
    }
    let fb = &info.framebuffer;
    let put =
        |m: &mut Vec<u8>, at: usize, bytes: &[u8]| m[at..at + bytes.len()].copy_from_slice(bytes);
    put(&mut m, 0x00, &1u32.to_le_bytes());
    put(&mut m, 0x04, &header_flags.to_le_bytes());
    put(&mut m, 0x08, &info.firmware_table_phys.to_le_bytes());
    put(&mut m, 0x10, &fb.phys_base.to_le_bytes());
    put(&mut m, 0x18, &fb.size.to_le_bytes());
    let words = [
        fb.width,
        fb.height,
        fb.stride,
        fb.format,
        fb.red_mask,
        fb.green_mask,
        fb.blue_mask,
        fb.reserved_mask,
    ];
    for (i, word) in words.iter().enumerate() {
        put(&mut m, 0x20 + 4 * i, &word.to_le_bytes());
    }
    put(&mut m, 0x40, &info.boot_image_size.to_le_bytes());
    put(&mut m, 0x58, &info.arch.to_le_bytes());
    put(&mut m, 0x5C, &framebuffer_pci.to_le_bytes());
    put(&mut m, 0x60, &info.boot_time.to_le_bytes());

    let cmdline_offset = m.len();
    m.extend_from_slice(cmdline);
    m.resize(m.len().next_multiple_of(8), 0);
    let map_offset = m.len();
    let mut count = 0u32;
    for e in memory_map {
        m.extend_from_slice(&e.phys_start.to_le_bytes());
        m.extend_from_slice(&e.page_count.to_le_bytes());
        m.extend_from_slice(&e.kind.to_le_bytes());
        m.extend_from_slice(&0u32.to_le_bytes());
        count += 1;
    }
    put(&mut m, 0x48, &(cmdline_offset as u32).to_le_bytes());
    put(&mut m, 0x4C, &(cmdline.len() as u32).to_le_bytes());
    put(&mut m, 0x50, &(map_offset as u32).to_le_bytes());
    put(&mut m, 0x54, &count.to_le_bytes());
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use boot_protocol::{Framebuffer, Kind};

    fn u32_at(m: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(m[at..at + 4].try_into().unwrap())
    }

    fn u64_at(m: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(m[at..at + 8].try_into().unwrap())
    }

    #[test]
    fn lays_out_the_message() {
        let info = BootInfo {
            arch: 1,
            flags: flags::FRAMEBUFFER_PRESENT | flags::RNG_SEED_VALID | flags::BOOT_TIME_VALID,
            firmware_table_phys: 0xE_0000,
            boot_image_size: 0x1234,
            boot_time: -7,
            framebuffer: Framebuffer {
                phys_base: 0x8000_0000,
                size: 0x30_0000,
                width: 1024,
                height: 768,
                stride: 1024,
                format: 2,
                red_mask: 0xFF_0000,
                green_mask: 0xFF00,
                blue_mask: 0xFF,
                reserved_mask: 0xFF00_0000,
            },
            framebuffer_pci_segment: 1,
            framebuffer_pci_bus: 3,
            framebuffer_pci_devfn: 2 * 8 + 1,
            framebuffer_pci_valid: 1,
            ..BootInfo::default()
        };
        let map = [
            MemoryMapEntry {
                phys_start: 0,
                page_count: 0x9F,
                kind: Kind::Usable as u32,
                reserved: 0,
            },
            MemoryMapEntry {
                phys_start: 0x10_0000,
                page_count: 3,
                kind: Kind::BootImage as u32,
                reserved: 9,
            },
        ];
        let m = message(&info, b"root=x", map.into_iter());
        assert_eq!(u32_at(&m, 0x00), 1);
        assert_eq!(
            u32_at(&m, 0x04),
            FRAMEBUFFER_PRESENT | BOOT_TIME_VALID | FRAMEBUFFER_PCI_VALID
        );
        assert_eq!(u64_at(&m, 0x08), 0xE_0000);
        assert_eq!(u64_at(&m, 0x10), 0x8000_0000);
        assert_eq!(u64_at(&m, 0x18), 0x30_0000);
        assert_eq!(u32_at(&m, 0x20), 1024);
        assert_eq!(u32_at(&m, 0x2C), 2);
        assert_eq!(u32_at(&m, 0x3C), 0xFF00_0000);
        assert_eq!(u64_at(&m, 0x40), 0x1234);
        assert_eq!((u32_at(&m, 0x48), u32_at(&m, 0x4C)), (0x68, 6));
        assert_eq!(&m[0x68..0x6E], b"root=x");
        assert_eq!((u32_at(&m, 0x50), u32_at(&m, 0x54)), (0x70, 2));
        assert_eq!(u32_at(&m, 0x58), 1);
        assert_eq!(u32_at(&m, 0x5C), 0x1_0311);
        assert_eq!(u64_at(&m, 0x60), (-7i64) as u64);
        assert_eq!(u64_at(&m, 0x88), 0x10_0000);
        assert_eq!(u64_at(&m, 0x90), 3);
        assert_eq!(u32_at(&m, 0x98), Kind::BootImage as u32);
        assert_eq!(u32_at(&m, 0x9C), 0);
        assert_eq!(m.len(), 0x70 + 2 * ENTRY_SIZE);
    }

    #[test]
    fn leaves_absent_fields_clear() {
        let m = message(&BootInfo::default(), b"", core::iter::empty());
        assert_eq!(u32_at(&m, 0x04), 0);
        assert_eq!((u32_at(&m, 0x48), u32_at(&m, 0x4C)), (0x68, 0));
        assert_eq!((u32_at(&m, 0x50), u32_at(&m, 0x54)), (0x68, 0));
        assert_eq!(m.len(), HEADER_SIZE);
        // A PCI function without a framebuffer is not given.
        let info = BootInfo {
            framebuffer_pci_bus: 3,
            framebuffer_pci_valid: 1,
            ..BootInfo::default()
        };
        let m = message(&info, b"", core::iter::empty());
        assert_eq!((u32_at(&m, 0x04), u32_at(&m, 0x5C)), (0, 0));
    }
}
