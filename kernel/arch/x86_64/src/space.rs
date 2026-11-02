//! The kernel's own address space: building its page tables at boot and
//! moving onto them, and the kernel dynamic region.

use crate::log::log;
use crate::memory::{self, FRAMES};
use crate::paging::{Flags, Mapper, PAGE_SIZE, PAT_VALUE, Policy, Tables};
use boot_protocol::{BootInfo, CMDLINE_MAX, Kind};
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};
use kernel_core::ranges::Ranges;
use kernel_core::sync::SpinLock;

/// The start of the kernel dynamic region.
const DYNAMIC_START: u64 = 0xFFFF_C000_0000_0000;
/// The end of the kernel dynamic region.
const DYNAMIC_END: u64 = 0xFFFF_FF80_0000_0000;
/// The size of the stack the kernel moves to.
const STACK_SIZE: u64 = 64 << 10;

const IA32_PAT: u32 = 0x277;

unsafe extern "C" {
    static __kernel_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __data_end: u8;
}

/// The direct map base, from BootInfo.
static DIRECT_MAP_BASE: AtomicU64 = AtomicU64::new(0);
/// The unused parts of the kernel dynamic region.
static DYNAMIC: SpinLock<Ranges<DYNAMIC_RANGES>> =
    SpinLock::new(Ranges::new(DYNAMIC_START, DYNAMIC_END));
/// The most free ranges the dynamic region keeps track of.
const DYNAMIC_RANGES: usize = 256;
/// The physical address of the kernel's top-level table.
static ROOT: AtomicU64 = AtomicU64::new(0);
/// The top of the stack the kernel moves to at boot.
static STACK_TOP: AtomicU64 = AtomicU64::new(0);

/// The top of the stack the kernel moved to at boot.
pub fn stack_top() -> u64 {
    STACK_TOP.load(Ordering::Relaxed)
}

/// The kernel's top-level table, once it runs on it.
pub fn root() -> u64 {
    ROOT.load(Ordering::Relaxed)
}

/// The virtual address of physical memory in the direct map.
pub fn direct(phys: u64) -> u64 {
    DIRECT_MAP_BASE.load(Ordering::Relaxed) + phys
}

/// Page table frames from the frame allocator, reached through the direct
/// map, which both the bootloader's and the kernel's tables contain at the
/// same base.
struct FrameTables;

impl Tables for FrameTables {
    fn alloc(&mut self) -> Option<u64> {
        FRAMES.lock().as_mut()?.alloc()
    }

    fn table(&mut self, phys: u64) -> &mut [u64; 512] {
        // SAFETY: the frame came from the allocator for a page table, so
        // nothing else uses it, and the direct map covers it.
        unsafe { &mut *(direct(phys) as *mut [u64; 512]) }
    }
}

/// Reserves `size` bytes of the dynamic region with an unmapped page
/// below them, and returns the start of the reserved range.
pub fn reserve_dynamic(size: u64) -> u64 {
    let size = size.next_multiple_of(PAGE_SIZE) + PAGE_SIZE;
    let start = DYNAMIC
        .lock()
        .alloc(size, PAGE_SIZE)
        .unwrap_or_else(|| panic!("the kernel dynamic region is full"));
    start + PAGE_SIZE
}

/// Returns a range from `reserve_dynamic`, with its guard page, to the
/// dynamic region. Its pages must be unmapped.
pub fn release_dynamic(start: u64, size: u64) {
    let size = size.next_multiple_of(PAGE_SIZE) + PAGE_SIZE;
    if let Err(e) = DYNAMIC.lock().free(start - PAGE_SIZE, size) {
        // The range stays reserved; only address space is lost.
        log!(Warning, "releasing kernel address space: {e}");
    }
}

/// Unmaps `size` bytes at `virt` in the kernel's tables, invalidates their
/// translations, and calls `unmapped` with the physical address and size
/// of each page.
fn unmap(virt: u64, size: u64, mut unmapped: impl FnMut(u64, u64)) {
    let mut tables = FrameTables;
    let mut mapper = Mapper::existing(&mut tables, root(), gib_pages());
    mapper
        .unmap(virt, size, |v, phys, page| {
            crate::cpu::invlpg(v);
            unmapped(phys, page);
        })
        .unwrap_or_else(|e| panic!("unmapping kernel memory: {e}"));
}

/// Maps `size` bytes of device memory at `phys` uncached into the dynamic
/// region and returns the address of `phys` there.
pub fn map_device(phys: u64, size: u64) -> u64 {
    let first = phys / PAGE_SIZE * PAGE_SIZE;
    let len = (phys + size).next_multiple_of(PAGE_SIZE) - first;
    let virt = reserve_dynamic(len);
    let flags = Flags {
        writable: true,
        executable: false,
        global: true,
        user: false,
        policy: Policy::UncachedDevice,
    };
    let mut tables = FrameTables;
    let mut mapper = Mapper::existing(&mut tables, root(), gib_pages());
    mapper
        .map(virt, first, len, flags)
        .unwrap_or_else(|e| panic!("mapping device memory: {e}"));
    virt + (phys - first)
}

/// Removes a mapping made by `map_device` with the same `phys` and `size`
/// and returns its address space.
#[expect(dead_code, reason = "no driver releases device memory yet")]
pub fn unmap_device(virt: u64, phys: u64, size: u64) {
    let first = phys / PAGE_SIZE * PAGE_SIZE;
    let len = (phys + size).next_multiple_of(PAGE_SIZE) - first;
    let start = virt - (phys - first);
    unmap(start, len, |_, _| {});
    release_dynamic(start, len);
}

/// Maps the page of physical memory at `phys` writable with `policy`
/// into the dynamic region and returns its address there, or `None`, with
/// nothing left reserved, when address space or page tables run out.
pub fn map_physical_page(phys: u64, policy: Policy) -> Option<u64> {
    let virt = DYNAMIC.lock().alloc(2 * PAGE_SIZE, PAGE_SIZE)? + PAGE_SIZE;
    let flags = Flags {
        writable: true,
        executable: false,
        global: true,
        user: false,
        policy,
    };
    let mut tables = FrameTables;
    let mut mapper = Mapper::existing(&mut tables, root(), gib_pages());
    if mapper.map(virt, phys, PAGE_SIZE, flags).is_err() {
        release_dynamic(virt, PAGE_SIZE);
        return None;
    }
    Some(virt)
}

/// Removes a mapping made by `map_physical_page` at `virt`.
pub fn unmap_physical_page(virt: u64) {
    unmap(virt, PAGE_SIZE, |_, _| {});
    release_dynamic(virt, PAGE_SIZE);
}

/// Maps `size` bytes of new memory, writable and not executable, with an
/// unmapped page below it, into the dynamic region and returns its start.
/// Returns `None`, with nothing left allocated, when memory or address
/// space runs out.
pub fn alloc_pages(size: u64) -> Option<u64> {
    let size = size.next_multiple_of(PAGE_SIZE);
    let virt = DYNAMIC.lock().alloc(size + PAGE_SIZE, PAGE_SIZE)? + PAGE_SIZE;
    let flags = Flags {
        writable: true,
        executable: false,
        global: true,
        user: false,
        policy: Policy::Cached,
    };
    let mut tables = FrameTables;
    let mut mapper = Mapper::existing(&mut tables, root(), gib_pages());
    let mut mapped = 0;
    while mapped < size {
        let Some(phys) = FRAMES.lock().as_mut().and_then(|f| f.alloc()) else {
            break;
        };
        if mapper.map(virt + mapped, phys, PAGE_SIZE, flags).is_err() {
            if let Some(frames) = FRAMES.lock().as_mut() {
                frames.free(phys, 1);
            }
            break;
        }
        mapped += PAGE_SIZE;
    }
    if mapped < size {
        if mapped > 0 {
            free_pages(virt, mapped);
        }
        return None;
    }
    Some(virt)
}

/// Frees `size` bytes from `alloc_pages` at `start`: unmaps them, frees
/// their frames and releases the address space.
pub fn free_pages(start: u64, size: u64) {
    let size = size.next_multiple_of(PAGE_SIZE);
    unmap(start, size, |phys, page| {
        if let Some(frames) = FRAMES.lock().as_mut() {
            frames.free(phys, page / PAGE_SIZE);
        }
    });
    release_dynamic(start, size);
}

/// Maps a new stack of `size` bytes, with an unmapped page below it, into
/// the dynamic region and returns its top.
pub fn new_stack(size: u64) -> u64 {
    let start =
        alloc_pages(size).unwrap_or_else(|| panic!("no memory for a stack of {size} bytes"));
    start + size
}

/// Frees a stack made by `new_stack` with the same `size`, given its top.
/// Nothing may run on it.
#[expect(dead_code, reason = "no thread exits yet")]
pub fn free_stack(top: u64, size: u64) {
    free_pages(top - size, size);
}

fn gib_pages() -> bool {
    core::arch::x86_64::__cpuid(0x8000_0001).edx & (1 << 26) != 0
}

fn symbol(s: *const u8) -> u64 {
    s as u64
}

/// The kinds of memory the kernel's direct map covers.
pub const DIRECT_KINDS: [Kind; 9] = [
    Kind::Usable,
    Kind::BootloaderReclaimable,
    Kind::KernelStack,
    Kind::BootImage,
    Kind::PageTables,
    Kind::AcpiReclaimable,
    Kind::AcpiNvs,
    Kind::FirmwareRuntime,
    Kind::ApTrampoline,
];

fn build(info: &BootInfo, mapper: &mut Mapper<FrameTables>) -> Result<u64, crate::paging::Error> {
    let data = Flags {
        writable: true,
        executable: false,
        global: true,
        user: false,
        policy: Policy::Cached,
    };

    // Adjacent ranges are mapped together so that large pages can span
    // range boundaries.
    let mut run: Option<(u64, u64)> = None;
    let direct =
        memory::memory_map(info).filter(|e| DIRECT_KINDS.iter().any(|&k| k as u32 == e.kind));
    for e in direct {
        let end = e.phys_start + e.page_count * PAGE_SIZE;
        run = match run {
            Some((start, run_end)) if run_end == e.phys_start => Some((start, end)),
            Some((start, run_end)) => {
                mapper.map(direct_of(info, start), start, run_end - start, data)?;
                Some((e.phys_start, end))
            }
            None => Some((e.phys_start, end)),
        };
    }
    if let Some((start, end)) = run {
        mapper.map(direct_of(info, start), start, end - start, data)?;
    }

    let phys = |virt: u64| info.kernel_phys_base + (virt - info.kernel_virt_base);
    let sections = [
        (
            &raw const __kernel_start,
            &raw const __text_end,
            false,
            true,
        ),
        (
            &raw const __rodata_start,
            &raw const __rodata_end,
            false,
            false,
        ),
        (&raw const __data_start, &raw const __data_end, true, false),
    ];
    for (start, end, writable, executable) in sections {
        let start = symbol(start) / PAGE_SIZE * PAGE_SIZE;
        let end = symbol(end).next_multiple_of(PAGE_SIZE);
        let flags = Flags {
            writable,
            executable,
            ..data
        };
        mapper.map(start, phys(start), end - start, flags)?;
    }

    let stack_phys = FRAMES
        .lock()
        .as_mut()
        .and_then(|f| f.alloc_contiguous(STACK_SIZE / PAGE_SIZE, 1))
        .ok_or(crate::paging::Error::OutOfMemory)?;
    let stack = reserve_dynamic(STACK_SIZE);
    mapper.map(stack, stack_phys, STACK_SIZE, data)?;
    Ok(stack)
}

fn direct_of(info: &BootInfo, phys: u64) -> u64 {
    info.direct_map_base + phys
}

// Loads CR3 from RDI and RSP from RSI, then enters the function in RDX
// with a zero return address, so that backtraces end there.
global_asm!(
    ".global kernel_switch",
    "kernel_switch:",
    "mov cr3, rdi",
    "mov rsp, rsi",
    "xor ebp, ebp",
    "push 0",
    "jmp rdx",
);

unsafe extern "sysv64" {
    fn kernel_switch(cr3: u64, rsp: u64, next: extern "sysv64" fn() -> !) -> !;
}

/// The BootInfo values the kernel keeps, copied before boot memory is
/// reclaimed.
pub static BOOT_INFO: SpinLock<Option<BootInfo>> = SpinLock::new(None);

/// The command line, copied before boot memory is reclaimed: its bytes and
/// their number.
pub static CMDLINE: SpinLock<([u8; CMDLINE_MAX], usize)> = SpinLock::new(([0; CMDLINE_MAX], 0));

fn copy_cmdline(info: &BootInfo) {
    let len = (info.cmdline_len as usize).min(CMDLINE_MAX);
    if len == 0 {
        return;
    }
    // SAFETY: the bootloader places the command line in BootloaderReclaimable
    // memory in the direct map, which is not reclaimed before this copy.
    let bytes = unsafe { core::slice::from_raw_parts(direct(info.cmdline_phys) as *const u8, len) };
    let mut copy = CMDLINE.lock();
    copy.0[..len].copy_from_slice(bytes);
    copy.1 = len;
}

/// Builds the kernel's page tables, moves onto them and onto a new stack,
/// and continues in `next`.
pub fn enter(info: BootInfo, next: extern "sysv64" fn() -> !) -> ! {
    DIRECT_MAP_BASE.store(info.direct_map_base, Ordering::Relaxed);
    copy_cmdline(&info);
    // SAFETY: IA32_PAT exists on every processor with PAT, which the
    // feature check requires. The new value keeps entries 0 to 3, which
    // the bootloader's tables can select, at the memory types of their
    // reset values.
    unsafe { crate::cpu::write_msr(IA32_PAT, PAT_VALUE) };

    let mut tables = FrameTables;
    let mut mapper = Mapper::new(&mut tables, gib_pages())
        .unwrap_or_else(|e| panic!("building the kernel page tables: {e}"));
    let stack = build(&info, &mut mapper)
        .unwrap_or_else(|e| panic!("building the kernel page tables: {e}"));
    let root = mapper.root();
    ROOT.store(root, Ordering::Relaxed);
    STACK_TOP.store(stack + STACK_SIZE, Ordering::Relaxed);
    crate::panic::set_stack(stack, stack + STACK_SIZE);
    *BOOT_INFO.lock() = Some(info);
    // SAFETY: the new tables map the kernel image at the same addresses,
    // the direct map at the same base, and the new stack; `next` never
    // returns, so nothing uses the old stack again.
    unsafe { kernel_switch(root, stack + STACK_SIZE, next) }
}

/// Adds the bootloader's memory and the boot stack to the frame allocator.
/// Runs on the kernel's tables and stack, after every BootInfo value the
/// kernel needs is copied.
pub fn reclaim_boot_memory(info: &BootInfo) -> u64 {
    let map = memory::memory_map(info);
    let mut frames = FRAMES.lock();
    let Some(frames) = frames.as_mut() else {
        return 0;
    };
    // The memory map lies in BootloaderReclaimable memory, which is freed
    // here; nothing allocates until the walk is done.
    frames.add_kind(map.clone(), Kind::BootloaderReclaimable);
    frames.add_kind(map.clone(), Kind::PageTables);
    // The kernel left the boot stack in `enter` and never returns to it.
    frames.add_kind(map, Kind::KernelStack);
    frames.free_count()
}
