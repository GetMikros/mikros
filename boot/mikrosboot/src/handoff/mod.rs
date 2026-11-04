//! What the kernel is given: the memory map and `BootInfo`, from where the
//! bootloader placed things.

use crate::elf::Kernel;
use crate::load::{self, Layout};
use crate::memmap::{self, Map, Region};
use crate::paging::BOOT_STACK_SIZE;
use boot_protocol::x86_64::{DIRECT_MAP_BASE, DIRECT_MAP_LIMIT};
use boot_protocol::{
    BOOT_INFO_SIZE, BootInfo, Framebuffer, Kind, MAGIC, MemoryMapEntry, PAGE_SIZE, VERSION_MAJOR,
    VERSION_MINOR, arch,
};

/// The pages allocated for `BootInfo` and the command line.
pub const INFO_PAGES: u64 = 2;
/// Where the command line is placed in the `BootInfo` allocation.
pub const CMDLINE_OFFSET: u64 = 0x100;

/// Memory allocated for the kernel, and what is passed to it. All
/// addresses are physical.
pub struct Prepared {
    pub kernel: Kernel,
    pub layout: Layout,
    pub kernel_phys: u64,
    pub boot_image_phys: u64,
    pub boot_image_size: u64,
    pub stack_phys: u64,
    pub trampoline: Option<u64>,
    /// The `INFO_PAGES` pages for `BootInfo` and the command line.
    pub info_phys: u64,
    pub cmdline_len: u64,
    /// The kernel memory map's entries.
    pub entries_phys: u64,
    pub entries_capacity: usize,
    /// The buffer for the final firmware memory map.
    pub firmware_map_phys: u64,
    pub firmware_map_size: usize,
    pub tables_phys: u64,
    /// The number of pages for page tables.
    pub tables: u64,
    pub framebuffer: Option<Framebuffer>,
    pub rsdp: u64,
    pub system_table: u64,
    pub rng_seed: Option<[u8; 32]>,
    pub boot_time: Option<u64>,
}

impl Prepared {
    /// The allocated ranges that get a kind of their own, and the
    /// framebuffer. The others keep the kind of the firmware's type.
    pub fn overlays(&self) -> [Option<Region>; 6] {
        [
            Some(Region {
                start: self.kernel_phys,
                pages: load::pages(self.layout.size),
                kind: Kind::KernelImage,
            }),
            Some(Region {
                start: self.boot_image_phys,
                pages: load::pages(self.boot_image_size).max(1),
                kind: Kind::BootImage,
            }),
            Some(Region {
                start: self.stack_phys,
                pages: BOOT_STACK_SIZE / PAGE_SIZE,
                kind: Kind::KernelStack,
            }),
            self.trampoline.map(|start| Region {
                start,
                pages: 1,
                kind: Kind::ApTrampoline,
            }),
            Some(Region {
                start: self.tables_phys,
                pages: self.tables,
                kind: Kind::PageTables,
            }),
            self.framebuffer
                .map(|fb| Region::covering(fb.phys_base, fb.size, Kind::Framebuffer)),
        ]
    }

    /// Builds the kernel memory map in `map` from the firmware's ranges, as
    /// start, page count and UEFI type, and `overlays`, and checks that the
    /// direct map can cover it.
    pub fn memory_map(
        &self,
        map: &mut Map,
        firmware: impl IntoIterator<Item = (u64, u64, u32)>,
    ) -> Result<(), memmap::Error> {
        for (start, pages, uefi_type) in firmware {
            map.push_firmware(start, pages, uefi_type)?;
        }
        map.sort()?;
        for region in self.overlays().into_iter().flatten() {
            map.overlay(region)?;
        }
        map.merge();
        map.check_direct_map_limit(DIRECT_MAP_LIMIT)
    }

    /// `BootInfo` for the memory map `entries`. An address of something
    /// absent is zero.
    pub fn boot_info(&self, entries: &[MemoryMapEntry]) -> BootInfo {
        let optional = crate::info::optional(self.framebuffer, self.rng_seed, self.boot_time);
        BootInfo {
            magic: MAGIC,
            version_major: VERSION_MAJOR,
            version_minor: VERSION_MINOR,
            size: BOOT_INFO_SIZE,
            arch: arch::X86_64,
            flags: optional.flags,
            memory_map_phys: entries.as_ptr() as u64,
            memory_map_count: entries.len() as u64,
            memory_map_entry_size: size_of::<MemoryMapEntry>() as u64,
            kernel_phys_base: self.kernel_phys,
            kernel_virt_base: self.layout.virt_base,
            kernel_size: self.layout.size,
            direct_map_base: DIRECT_MAP_BASE,
            boot_image_phys: self.boot_image_phys,
            boot_image_size: self.boot_image_size,
            cmdline_phys: if self.cmdline_len == 0 {
                0
            } else {
                self.info_phys + CMDLINE_OFFSET
            },
            cmdline_len: self.cmdline_len,
            firmware_table_phys: self.rsdp,
            uefi_system_table_phys: self.system_table,
            framebuffer: optional.framebuffer,
            rng_seed: optional.rng_seed,
            boot_time: optional.boot_time,
            arch_data0: self.trampoline.unwrap_or(0),
            arch_data1: 0,
        }
    }
}

#[cfg(test)]
mod tests;
