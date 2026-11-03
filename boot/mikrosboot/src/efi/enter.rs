//! Allocating the kernel's memory, exiting boot services and entering the
//! kernel.

use super::Failure;
use super::gather::Gathered;
use alloc::vec;
use boot_protocol::x86_64::{AP_TRAMPOLINE_END, DIRECT_MAP_BASE, fits_ap_trampoline};
use boot_protocol::{BootInfo, Kind, MemoryMapEntry, PAGE_SIZE};
use core::arch::{asm, global_asm};
use core::ptr::NonNull;
use mikrosboot::exit::{self, FinalMap};
use mikrosboot::handoff::{CMDLINE_OFFSET, INFO_PAGES, Prepared};
use mikrosboot::load::{self, PHYS_ALIGN};
use mikrosboot::memmap::{Map, Region};
use mikrosboot::paging::{self, BOOT_STACK_SIZE, Frames, Placement};
use uefi::boot::{self, AllocateType, MemoryType};
use uefi::mem::memory_map::MemoryMap;
use uefi::{Status, table};
use uefi_raw::table::boot::{BootServices, MemoryDescriptor};

/// Room for firmware memory map descriptors beyond those of the map read
/// while allocating.
const DESCRIPTOR_SLACK: usize = 64;
/// Room for tables beyond `paging::table_bound`, in case the final memory
/// map has more direct-mapped runs than the one the bound was taken from.
const TABLE_SLACK: u64 = 16;
/// Kernel memory map entries beyond the firmware's: each of the ranges
/// overlaid on the firmware map can split one range in three.
const ENTRY_SLACK: usize = 32;

// Switches to the kernel's page tables and stack and jumps to the kernel.
// Arguments: RDI = CR3, RSI = RSP, RDX = BootInfo address, RCX = entry.
global_asm!(
    ".global mikros_enter",
    ".global mikros_enter_end",
    ".p2align 4",
    "mikros_enter:",
    "cli",
    "cld",
    "mov r8, rdx",
    "mov r9, rcx",
    // Set IA32_EFER.NXE before loading tables that use the NX bit.
    "mov ecx, 0xC0000080",
    "rdmsr",
    "or eax, 0x800",
    "wrmsr",
    // Set CR0.WP.
    "mov rax, cr0",
    "or rax, 0x10000",
    "mov cr0, rax",
    "mov cr3, rdi",
    "mov rsp, rsi",
    "xor ebp, ebp",
    "mov rdi, r8",
    "jmp r9",
    "mikros_enter_end:",
);

unsafe extern "sysv64" {
    fn mikros_enter(cr3: u64, rsp: u64, boot_info: u64, entry: u64) -> !;
}

unsafe extern "C" {
    static mikros_enter_end: u8;
}

/// The physical pages holding `mikros_enter`, as start and size.
fn switch_code() -> (u64, u64) {
    let start = mikros_enter as *const () as u64;
    let end = (&raw const mikros_enter_end) as u64;
    let first = start / PAGE_SIZE * PAGE_SIZE;
    (first, end.next_multiple_of(PAGE_SIZE) - first)
}

/// Stops the processor with interrupts disabled.
fn halt() -> ! {
    loop {
        // SAFETY: stopping the processor has no memory effects.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

fn five_level_paging() -> bool {
    let cr4: u64;
    // SAFETY: reading CR4 has no side effects at the bootloader's privilege.
    unsafe { asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack)) };
    cr4 & (1 << 12) != 0
}

fn gib_pages_supported() -> bool {
    let max = core::arch::x86_64::__cpuid(0x8000_0000).eax;
    max >= 0x8000_0001 && core::arch::x86_64::__cpuid(0x8000_0001).edx & (1 << 26) != 0
}

fn allocate(ty: AllocateType, pages: u64, step: &'static str) -> Result<u64, Failure> {
    let ptr = boot::allocate_pages(ty, MemoryType::LOADER_DATA, pages as usize)
        .map_err(|e| Failure::new(step, e))?;
    // SAFETY: the allocation is `pages` pages long and owned by us.
    unsafe { ptr.as_ptr().write_bytes(0, (pages * PAGE_SIZE) as usize) };
    Ok(ptr.as_ptr() as u64)
}

fn free(phys: u64, pages: u64) {
    if let (Some(ptr), true) = (NonNull::new(phys as *mut u8), pages > 0) {
        // SAFETY: the pages are part of an allocation of ours that nothing
        // else refers to.
        let _ = unsafe { boot::free_pages(ptr, pages as usize) };
    }
}

/// Allocates `pages` pages whose start is aligned to `PHYS_ALIGN`.
fn allocate_aligned(pages: u64, step: &'static str) -> Result<u64, Failure> {
    let extra = PHYS_ALIGN / PAGE_SIZE - 1;
    let raw = allocate(AllocateType::AnyPages, pages + extra, step)?;
    let aligned = raw.next_multiple_of(PHYS_ALIGN);
    let head = (aligned - raw) / PAGE_SIZE;
    free(raw, head);
    free(aligned + pages * PAGE_SIZE, extra - head);
    Ok(aligned)
}

/// The direct-mapped runs of the current firmware memory map, and the size
/// of that map in descriptors.
fn current_runs(step: &'static str) -> Result<(alloc::vec::Vec<(u64, u64)>, usize), Failure> {
    let map = boot::memory_map(MemoryType::LOADER_DATA).map_err(|e| Failure::new(step, e))?;
    let mut buf = vec![MemoryMapEntry::default(); map.len()];
    let mut entries = Map::new(&mut buf);
    for d in map.entries() {
        entries
            .push_firmware(d.phys_start, d.page_count, d.ty.0)
            .map_err(|e| Failure::new(step, e))?;
    }
    entries.sort().map_err(|e| Failure::new(step, e))?;
    let runs = paging::direct_runs(entries.entries()).collect();
    Ok((runs, map.len()))
}

fn prepare(g: Gathered) -> Result<Prepared, Failure> {
    let step = "allocating memory for the kernel";
    let kernel_pages = load::pages(g.layout.size);
    let kernel_phys = allocate_aligned(kernel_pages, step)?;
    // SAFETY: the allocation is `layout.size` bytes long and owned by us.
    let range =
        unsafe { core::slice::from_raw_parts_mut(kernel_phys as *mut u8, g.layout.size as usize) };
    load::copy(&g.kernel_file, &g.kernel, g.layout, range);

    let step = "allocating memory for the boot image";
    let boot_image_size = g.boot_image.len() as u64;
    let boot_image_phys = allocate(
        AllocateType::AnyPages,
        load::pages(boot_image_size).max(1),
        step,
    )?;
    // SAFETY: the allocation holds at least `boot_image_size` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(
            g.boot_image.as_ptr(),
            boot_image_phys as *mut u8,
            g.boot_image.len(),
        )
    };

    let step = "allocating memory for the boot stack";
    let stack_phys = allocate(AllocateType::AnyPages, BOOT_STACK_SIZE / PAGE_SIZE, step)?;
    // The allocation has no lower bound, but the uefi crate never returns
    // page 0, the only page below the trampoline's; a page outside its
    // bounds is not used all the same.
    let trampoline = match allocate(
        AllocateType::MaxAddress(AP_TRAMPOLINE_END - 1),
        1,
        "allocating the AP trampoline page",
    ) {
        Ok(page) if fits_ap_trampoline(page) => Some(page),
        Ok(page) => {
            free(page, 1);
            None
        }
        Err(_) => None,
    };

    let step = "allocating memory for the boot information";
    let info_phys = allocate(AllocateType::AnyPages, INFO_PAGES, step)?;
    let cmdline_len = g.cmdline.len() as u64;
    // SAFETY: the command line is at most CMDLINE_MAX bytes, which fit after
    // CMDLINE_OFFSET in the INFO_PAGES pages.
    unsafe {
        core::ptr::copy_nonoverlapping(
            g.cmdline.as_ptr(),
            (info_phys + CMDLINE_OFFSET) as *mut u8,
            g.cmdline.len(),
        )
    };

    let step = "allocating memory for page tables";
    let (runs, descriptors) = current_runs(step)?;
    let tables =
        paging::table_bound(&g.kernel, switch_code().1, runs.iter().copied()) + TABLE_SLACK;
    let tables_phys = allocate(AllocateType::AnyPages, tables, step)?;
    drop(runs);

    let step = "allocating memory for the memory map";
    let descriptor_size = size_of::<MemoryDescriptor>().max(48);
    let firmware_map_size = (descriptors + DESCRIPTOR_SLACK) * descriptor_size;
    let firmware_map_phys = allocate(
        AllocateType::AnyPages,
        load::pages(firmware_map_size as u64),
        step,
    )?;
    let entries_capacity = descriptors + DESCRIPTOR_SLACK + ENTRY_SLACK;
    let entries_size = (entries_capacity * size_of::<MemoryMapEntry>()) as u64;
    let entries_phys = allocate(AllocateType::AnyPages, load::pages(entries_size), step)?;

    Ok(Prepared {
        kernel: g.kernel,
        layout: g.layout,
        kernel_phys,
        boot_image_phys,
        boot_image_size,
        stack_phys,
        trampoline,
        info_phys,
        cmdline_len,
        entries_phys,
        entries_capacity,
        firmware_map_phys,
        firmware_map_size,
        tables_phys,
        tables,
        framebuffer: g.framebuffer,
        rsdp: g.rsdp,
        system_table: table::system_table_raw().map_or(0, |st| st.as_ptr() as u64),
        rng_seed: g.rng_seed,
        boot_time: g.boot_time,
    })
}

/// The boot services used to exit them, with the buffer for the memory map.
struct Exit<'a> {
    bt: &'a BootServices,
    image: *mut core::ffi::c_void,
    buffer: u64,
    capacity: usize,
}

impl exit::BootServices for Exit<'_> {
    type Error = Status;

    fn memory_map(&mut self) -> Result<(FinalMap, usize), Status> {
        let mut size = self.capacity;
        let (mut key, mut descriptor_size, mut version) = (0, 0, 0);
        // SAFETY: the buffer is `capacity` bytes and ours.
        let status = unsafe {
            (self.bt.get_memory_map)(
                &mut size,
                self.buffer as *mut MemoryDescriptor,
                &mut key,
                &mut descriptor_size,
                &mut version,
            )
        };
        if status != Status::SUCCESS {
            return Err(status);
        }
        let map = FinalMap {
            size,
            descriptor_size,
        };
        Ok((map, key))
    }

    fn exit(&mut self, key: usize) -> bool {
        // SAFETY: the image handle and key are the firmware's own values.
        unsafe { (self.bt.exit_boot_services)(self.image, key) == Status::SUCCESS }
    }
}

/// Reads the memory map and exits boot services, retrying when the map key
/// is stale. Gives `Err` with a failure only when boot services can still
/// be used to report it.
fn exit_boot_services(p: &Prepared) -> Result<FinalMap, Option<Failure>> {
    let st = table::system_table_raw().ok_or(None)?;
    // SAFETY: the system table is valid until boot services end, and its
    // boot services pointer stays valid until ExitBootServices succeeds.
    let bt: &BootServices = unsafe { &*st.as_ref().boot_services };
    let mut services = Exit {
        bt,
        image: boot::image_handle().as_ptr(),
        buffer: p.firmware_map_phys,
        capacity: p.firmware_map_size,
    };
    exit::exit(&mut services)
        .map_err(|e| e.map(|status| Failure::new("reading the final memory map", status)))
}

struct Pool {
    next: u64,
    end: u64,
}

impl Frames for Pool {
    fn alloc(&mut self) -> Option<u64> {
        (self.next < self.end).then(|| {
            self.next += PAGE_SIZE;
            self.next - PAGE_SIZE
        })
    }

    fn table(&mut self, phys: u64) -> &mut [u64; 512] {
        // SAFETY: the frame is in the table pool, which the firmware's
        // identity map covers and nothing else uses.
        unsafe { &mut *(phys as *mut [u64; 512]) }
    }
}

/// Builds the kernel memory map and page tables, fills `BootInfo` and
/// enters the kernel. Runs after boot services have ended.
fn enter(p: &Prepared, fw: &FinalMap) -> Option<core::convert::Infallible> {
    // SAFETY: the buffer was allocated for `entries_capacity` entries.
    let buf = unsafe {
        core::slice::from_raw_parts_mut(p.entries_phys as *mut MemoryMapEntry, p.entries_capacity)
    };
    let mut map = Map::new(buf);
    let firmware = (0..fw.size / fw.descriptor_size).map(|i| {
        let at = p.firmware_map_phys as usize + i * fw.descriptor_size;
        // SAFETY: the firmware wrote `size` bytes of descriptors here.
        let d = unsafe { (at as *const MemoryDescriptor).read_unaligned() };
        (d.phys_start, d.page_count, d.ty.0)
    });
    p.memory_map(&mut map, firmware).ok()?;

    let tables_end = p.tables_phys + p.tables * PAGE_SIZE;
    let mut pool = Pool {
        next: p.tables_phys,
        end: tables_end,
    };
    let placement = Placement {
        kernel: &p.kernel,
        layout: p.layout,
        kernel_phys: p.kernel_phys,
        stack_phys: p.stack_phys,
        memory_map: map.entries(),
        direct_map_base: DIRECT_MAP_BASE,
        switch_code: switch_code(),
    };
    let cr3 = paging::build(&mut pool, gib_pages_supported(), &placement).ok()?;
    // Pool frames that hold no table go back to the bootloader's kind.
    if pool.next < tables_end {
        map.overlay(Region {
            start: pool.next,
            pages: (tables_end - pool.next) / PAGE_SIZE,
            kind: Kind::BootloaderReclaimable,
        })
        .ok()?;
        map.merge();
    }

    let info = p.boot_info(map.entries());
    // SAFETY: the BootInfo allocation is ours, page-aligned and INFO_PAGES
    // pages.
    unsafe { (p.info_phys as *mut BootInfo).write(info) };

    // The stack was zeroed when it was allocated, so the top 8 bytes are
    // the zero value the kernel finds at RSP.
    let rsp = paging::stack_virt(p.layout) + BOOT_STACK_SIZE - 8;
    // SAFETY: the tables map the kernel, the stack, BootInfo in the direct
    // map, and mikros_enter at its physical address.
    unsafe { mikros_enter(cr3, rsp, DIRECT_MAP_BASE + p.info_phys, p.kernel.entry) }
}

pub fn boot(g: Gathered) -> Failure {
    if five_level_paging() {
        return Failure::new("checking the paging mode", "5-level paging is enabled");
    }
    let prepared = match prepare(g) {
        Ok(p) => p,
        Err(failure) => return failure,
    };
    match exit_boot_services(&prepared) {
        Ok(fw) => {
            // SAFETY: boot services have ended; nothing may interrupt now.
            unsafe { asm!("cli", options(nomem, nostack)) };
            enter(&prepared, &fw);
            halt()
        }
        Err(Some(failure)) => failure,
        Err(None) => halt(),
    }
}
