//! Kernel page-table isolation: the kernel half of the tables that user
//! mode runs on, which maps only the code that enters and leaves the
//! kernel, the entry data it reads, the IDT, and each processor's entry
//! data, GDT, TSS, entry stack and IST stacks, at the addresses the
//! kernel's tables map them at.
//!
//! Every processor adds its part while it starts, before any address
//! space exists, and each address space's tables for user mode copy the
//! kernel half's top-level entries when they are made.

use crate::mitigations;
use crate::paging::{Flags, Mapper, PAGE_SIZE, Policy, Tables};
use crate::percpu::{self, PerCpu};
use crate::space::{self, FrameTables};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

unsafe extern "C" {
    static __entry_text_start: u8;
    static __entry_text_end: u8;
    static __entry_data_start: u8;
    static __entry_data_end: u8;
}

/// The top-level table whose kernel half user mode runs with, or 0. Only
/// one processor adds to it at a time: the boot processor, and then each
/// processor it starts, while it waits.
static ROOT: AtomicU64 = AtomicU64::new(0);
/// Set once tables for user mode have copied the template's entries,
/// after which it must not gain any.
static COPIED: AtomicBool = AtomicBool::new(false);

/// Maps `size` bytes of the kernel's memory from `virt` into the template
/// `root` at the same addresses, as the kernel's tables map them.
fn share(root: u64, virt: u64, size: u64, writable: bool, executable: bool) {
    let flags = Flags {
        writable,
        executable,
        global: false,
        user: false,
        policy: Policy::Cached,
    };
    for page in (virt..virt + size).step_by(PAGE_SIZE as usize) {
        let phys = Mapper::existing(&mut FrameTables, space::root(), false)
            .translate(page)
            .unwrap_or_else(|| panic!("isolation: {page:#x} is not mapped"));
        Mapper::existing(&mut FrameTables, root, false)
            .map(page, phys, PAGE_SIZE, flags)
            .unwrap_or_else(|e| panic!("isolation: mapping {page:#x}: {e}"));
    }
}

/// Adds what processor `cpu` enters and leaves the kernel with to the
/// kernel half of the tables for user mode: its entry data and the IST
/// stacks whose tops are given, and, on the first call, the entry code
/// and data and the IDT. Does nothing without kernel page-table
/// isolation.
pub fn add_cpu(cpu: &PerCpu, ist_tops: [u64; 4]) {
    if !mitigations::isolation() {
        return;
    }
    assert!(
        !COPIED.load(Ordering::Acquire),
        "isolation: processor added after tables for user mode were made"
    );
    let mut root = ROOT.load(Ordering::Acquire);
    if root == 0 {
        root = Mapper::new(&mut FrameTables, false)
            .unwrap_or_else(|e| panic!("isolation: {e}"))
            .root();
        let range = |start: &u8, end: &u8| {
            let start = start as *const u8 as u64;
            (start, end as *const u8 as u64 - start)
        };
        // SAFETY: the linker script defines the four symbols.
        let (text, text_size) = range(unsafe { &__entry_text_start }, unsafe { &__entry_text_end });
        // SAFETY: as above.
        let (data, data_size) = range(unsafe { &__entry_data_start }, unsafe { &__entry_data_end });
        share(root, text, text_size, false, true);
        share(root, data, data_size, false, false);
        share(root, crate::idt::table(), PAGE_SIZE, false, false);
        ROOT.store(root, Ordering::Release);
    }
    share(
        root,
        cpu as *const PerCpu as u64,
        percpu::ENTRY_SIZE as u64,
        true,
        false,
    );
    for top in ist_tops {
        let size = crate::gdt::IST_STACK_SIZE;
        share(root, top - size, size, true, false);
    }
    // The processor runs on the kernel's own tables until it loads an
    // address space's.
    cpu.kernel_cr3.store(space::root(), Ordering::Relaxed);
    cpu.user_cr3.store(root, Ordering::Relaxed);
}

/// The tables for user mode that map nothing in the lower half, for a
/// processor that has the kernel's own tables loaded: user mode, which a
/// thread being ended may still return to, faults on its first access.
pub fn empty_user_root() -> u64 {
    ROOT.load(Ordering::Acquire)
}

/// Fills the top-level table at `root`, from `tables`, as tables for user
/// mode: no lower half, which the address space's tables give it, and the
/// template's kernel half.
pub fn fill_user_root(tables: &mut impl Tables, root: u64) {
    COPIED.store(true, Ordering::Release);
    let template = ROOT.load(Ordering::Acquire);
    let mut upper = [0; 256];
    upper.copy_from_slice(&FrameTables.table(template)[256..]);
    let table = tables.table(root);
    table[..256].fill(0);
    table[256..].copy_from_slice(&upper);
}
