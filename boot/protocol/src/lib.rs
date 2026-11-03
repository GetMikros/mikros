//! The data passed from the Mikros bootloader to the kernel.
//!
//! All structures are little-endian and laid out with `repr(C)`; the tests
//! check every field offset.

#![no_std]

/// The value of `BootInfo::magic`.
pub const MAGIC: [u8; 8] = *b"MIKROSBI";
/// The protocol version this crate describes.
pub const VERSION_MAJOR: u16 = 1;
pub const VERSION_MINOR: u16 = 0;

/// The name and type of the ELF note that gives the protocol version a
/// kernel was built for. Its descriptor is a `u16` major version followed
/// by a `u16` minor version.
pub const NOTE_NAME: &[u8] = b"Mikros";
pub const NOTE_TYPE: u32 = 1;

/// Values of `BootInfo::arch`.
pub mod arch {
    pub const X86_64: u32 = 1;
    pub const AARCH64: u32 = 2;
}

/// Bits of `BootInfo::flags`.
pub mod flags {
    pub const FRAMEBUFFER_PRESENT: u32 = 1 << 0;
    pub const RNG_SEED_VALID: u32 = 1 << 1;
    pub const BOOT_TIME_VALID: u32 = 1 << 2;
}

/// Values of `Framebuffer::format`.
pub mod format {
    /// Bytes of a pixel are red, green, blue, reserved.
    pub const RGB: u32 = 1;
    /// Bytes of a pixel are blue, green, red, reserved.
    pub const BGR: u32 = 2;
    /// The masks define the layout.
    pub const BITMASK: u32 = 3;
}

/// The longest command line, in bytes.
pub const CMDLINE_MAX: usize = 4096;

/// The size of a page, in bytes.
pub const PAGE_SIZE: u64 = 4096;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BootInfo {
    pub magic: [u8; 8],
    pub version_major: u16,
    pub version_minor: u16,
    pub size: u32,
    pub arch: u32,
    pub flags: u32,
    pub memory_map_phys: u64,
    pub memory_map_count: u64,
    pub memory_map_entry_size: u64,
    pub kernel_phys_base: u64,
    pub kernel_virt_base: u64,
    pub kernel_size: u64,
    pub direct_map_base: u64,
    pub boot_image_phys: u64,
    pub boot_image_size: u64,
    pub cmdline_phys: u64,
    pub cmdline_len: u64,
    pub firmware_table_phys: u64,
    pub uefi_system_table_phys: u64,
    pub framebuffer: Framebuffer,
    pub rng_seed: [u8; 32],
    pub boot_time: i64,
    pub arch_data0: u64,
    pub arch_data1: u64,
}

/// The value of `BootInfo::size` for this version.
pub const BOOT_INFO_SIZE: u32 = 0xE8;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Framebuffer {
    pub phys_base: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    /// Pixels per line in memory.
    pub stride: u32,
    pub format: u32,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub reserved_mask: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryMapEntry {
    pub phys_start: u64,
    /// Number of 4096-byte pages.
    pub page_count: u64,
    pub kind: u32,
    pub reserved: u32,
}

/// The kinds of memory map ranges.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Usable = 1,
    BootloaderReclaimable = 2,
    KernelImage = 3,
    KernelStack = 4,
    BootImage = 5,
    PageTables = 6,
    AcpiReclaimable = 7,
    AcpiNvs = 8,
    FirmwareRuntime = 9,
    Mmio = 10,
    Framebuffer = 11,
    ApTrampoline = 12,
    Reserved = 13,
    Bad = 14,
}

impl Kind {
    pub const fn from_u32(value: u32) -> Option<Kind> {
        Some(match value {
            1 => Kind::Usable,
            2 => Kind::BootloaderReclaimable,
            3 => Kind::KernelImage,
            4 => Kind::KernelStack,
            5 => Kind::BootImage,
            6 => Kind::PageTables,
            7 => Kind::AcpiReclaimable,
            8 => Kind::AcpiNvs,
            9 => Kind::FirmwareRuntime,
            10 => Kind::Mmio,
            11 => Kind::Framebuffer,
            12 => Kind::ApTrampoline,
            13 => Kind::Reserved,
            14 => Kind::Bad,
            _ => return None,
        })
    }

    /// Whether ranges of this kind are covered by the direct map.
    pub const fn in_direct_map(self) -> bool {
        matches!(self as u32, 1..=9 | 12)
    }
}

/// Addresses of the x86_64 boot protocol.
pub mod x86_64 {
    /// The lowest address of the kernel image range, which runs to the top
    /// of the address space.
    pub const KERNEL_RANGE_START: u64 = 0xFFFF_FFFF_8000_0000;
    pub const DIRECT_MAP_BASE: u64 = 0xFFFF_8000_0000_0000;
    /// Ranges covered by the direct map must end at or below this physical
    /// address.
    pub const DIRECT_MAP_LIMIT: u64 = 64 << 40;
    /// The lowest and highest-plus-one physical addresses of the AP
    /// trampoline page.
    pub const AP_TRAMPOLINE_MIN: u64 = 0x1000;
    pub const AP_TRAMPOLINE_END: u64 = 0xA0000;

    /// Whether the page at `phys` lies between `AP_TRAMPOLINE_MIN` and
    /// `AP_TRAMPOLINE_END`, so that it may hold the AP trampoline.
    pub const fn fits_ap_trampoline(phys: u64) -> bool {
        phys >= AP_TRAMPOLINE_MIN
            && phys.is_multiple_of(super::PAGE_SIZE)
            && phys <= AP_TRAMPOLINE_END - super::PAGE_SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn boot_info_has_the_version_1_0_layout() {
        assert_eq!(size_of::<BootInfo>(), BOOT_INFO_SIZE as usize);
        assert_eq!(align_of::<BootInfo>(), 8);
        let offsets = [
            (offset_of!(BootInfo, magic), 0x00),
            (offset_of!(BootInfo, version_major), 0x08),
            (offset_of!(BootInfo, version_minor), 0x0A),
            (offset_of!(BootInfo, size), 0x0C),
            (offset_of!(BootInfo, arch), 0x10),
            (offset_of!(BootInfo, flags), 0x14),
            (offset_of!(BootInfo, memory_map_phys), 0x18),
            (offset_of!(BootInfo, memory_map_count), 0x20),
            (offset_of!(BootInfo, memory_map_entry_size), 0x28),
            (offset_of!(BootInfo, kernel_phys_base), 0x30),
            (offset_of!(BootInfo, kernel_virt_base), 0x38),
            (offset_of!(BootInfo, kernel_size), 0x40),
            (offset_of!(BootInfo, direct_map_base), 0x48),
            (offset_of!(BootInfo, boot_image_phys), 0x50),
            (offset_of!(BootInfo, boot_image_size), 0x58),
            (offset_of!(BootInfo, cmdline_phys), 0x60),
            (offset_of!(BootInfo, cmdline_len), 0x68),
            (offset_of!(BootInfo, firmware_table_phys), 0x70),
            (offset_of!(BootInfo, uefi_system_table_phys), 0x78),
            (offset_of!(BootInfo, framebuffer), 0x80),
            (offset_of!(BootInfo, rng_seed), 0xB0),
            (offset_of!(BootInfo, boot_time), 0xD0),
            (offset_of!(BootInfo, arch_data0), 0xD8),
            (offset_of!(BootInfo, arch_data1), 0xE0),
        ];
        for (i, (actual, expected)) in offsets.into_iter().enumerate() {
            assert_eq!(actual, expected, "field {i}");
        }
    }

    #[test]
    fn framebuffer_and_entries_have_their_layouts() {
        assert_eq!(size_of::<Framebuffer>(), 48);
        assert_eq!(offset_of!(Framebuffer, width), 0x10);
        assert_eq!(offset_of!(Framebuffer, format), 0x1C);
        assert_eq!(offset_of!(Framebuffer, reserved_mask), 0x2C);
        assert_eq!(size_of::<MemoryMapEntry>(), 24);
        assert_eq!(align_of::<MemoryMapEntry>(), 8);
        assert_eq!(offset_of!(MemoryMapEntry, kind), 0x10);
    }

    #[test]
    fn kinds_round_trip_and_know_the_direct_map() {
        for value in 1..=14 {
            assert_eq!(Kind::from_u32(value).map(|k| k as u32), Some(value));
        }
        assert_eq!(Kind::from_u32(0), None);
        assert_eq!(Kind::from_u32(15), None);
        assert!(Kind::Usable.in_direct_map() && Kind::FirmwareRuntime.in_direct_map());
        assert!(Kind::ApTrampoline.in_direct_map());
        for kind in [Kind::Mmio, Kind::Framebuffer, Kind::Reserved, Kind::Bad] {
            assert!(!kind.in_direct_map(), "{kind:?}");
        }
    }

    #[test]
    fn bounds_the_ap_trampoline_page() {
        use x86_64::fits_ap_trampoline;
        assert!(fits_ap_trampoline(0x1000) && fits_ap_trampoline(0x9F000));
        for phys in [0, 0xA0000, 0x1800, 0xFFFF_FFFF_FFFF_F000] {
            assert!(!fits_ap_trampoline(phys), "{phys:#x}");
        }
    }
}
