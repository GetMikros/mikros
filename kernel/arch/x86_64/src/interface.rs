//! The x86_64 implementation of the kernel core's architecture interface.

use crate::page_tables::SpaceTables;
use crate::thread_arch::ArchThread;
use crate::{context, cpu, idt, percpu, serial, space, time, timer};
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::arch::asm;
use core::sync::atomic::Ordering;
use kernel_core::arch::Arch;

/// The interrupt flag in RFLAGS.
const RFLAGS_IF: u64 = 1 << 9;

/// The x86_64 architecture.
pub struct X86;

impl Arch for X86 {
    fn halt() -> ! {
        cpu::halt()
    }

    fn wait_for_interrupt() {
        // SAFETY: with interrupts enabled for `hlt`, pending interrupts
        // are taken and handled before they are disabled again; this
        // touches no memory.
        unsafe { asm!("sti", "hlt", "cli", options(nomem, nostack)) };
    }

    fn interrupts_enabled() -> bool {
        cpu::rflags() & RFLAGS_IF != 0
    }

    fn interrupts_disable() -> bool {
        let enabled = Self::interrupts_enabled();
        // SAFETY: disabling interrupts touches no memory.
        unsafe { asm!("cli", options(nomem, nostack)) };
        enabled
    }

    unsafe fn interrupts_restore(enabled: bool) {
        if enabled {
            // SAFETY: the caller guarantees nothing still needs interrupts
            // disabled.
            unsafe { asm!("sti", options(nomem, nostack)) };
        }
    }

    fn handling_interrupt() -> bool {
        idt::handling()
    }

    fn cpu_index() -> u32 {
        percpu::index()
    }

    fn cpu_data() -> *const () {
        if !percpu::READY.load(Ordering::Acquire) {
            return core::ptr::null();
        }
        percpu::this().core_data.load(Ordering::Acquire)
    }

    unsafe fn set_cpu_data(data: *const ()) {
        percpu::this()
            .core_data
            .store(data.cast_mut(), Ordering::Release);
    }

    fn monotonic_now() -> i64 {
        time::now()
    }

    fn clock_maintenance_interval() -> Option<i64> {
        time::maintenance_interval()
    }

    fn clock_maintain() {
        time::maintain();
    }

    fn cycle_counter() -> u64 {
        cpu::rdtsc()
    }

    fn random_source_present() -> bool {
        crate::entropy::present()
    }

    fn random_seed(out: &mut [u8]) -> usize {
        crate::entropy::seed(out)
    }

    fn timer_arm(deadline: i64) {
        timer::arm(deadline);
    }

    fn timer_disarm() {
        timer::disarm();
    }

    fn console_write(line: &[u8]) {
        crate::console::write(line);
    }

    fn frame_address(phys: u64) -> u64 {
        space::direct(phys)
    }

    fn physical_page_map(phys: u64, policy: kernel_core::arch::CachePolicy) -> Option<u64> {
        space::map_physical_page(phys, policy)
    }

    unsafe fn physical_page_unmap(virt: u64) {
        space::unmap_physical_page(virt);
    }

    fn kernel_frames_map(frames: &[u64]) -> Option<u64> {
        space::map_frames(frames)
    }

    unsafe fn kernel_frames_unmap(virt: u64, len: u64) {
        space::unmap_frames(virt, len);
    }

    fn coverage_point(area: u64, capacity: u64) {
        // Only the fuzz build has the hook.
        #[cfg(feature = "fuzz")]
        crate::coverage_hook::point(area, capacity);
        #[cfg(not(feature = "fuzz"))]
        let _ = (area, capacity);
    }

    fn kernel_pages_alloc(size: u64) -> Option<u64> {
        space::alloc_pages(size)
    }

    unsafe fn kernel_pages_free(start: u64, size: u64) {
        space::free_pages(start, size);
    }

    fn debug_write(bytes: &[u8]) {
        serial::write(bytes);
    }

    unsafe fn context_prepare(stack_top: u64, entry: u64, arg: u64) -> u64 {
        // SAFETY: the caller guarantees the bytes below `stack_top` are
        // mapped and unused.
        unsafe { context::prepare(stack_top, entry, arg) }
    }

    unsafe fn context_switch(save: *mut u64, load: u64) {
        // SAFETY: the caller guarantees `load` is a switchable context.
        unsafe { context::switch(save, load) };
    }

    fn thread_new() -> *mut () {
        let Some(thread) = ArchThread::new() else {
            return core::ptr::null_mut();
        };
        match kernel_core::fallible::try_box(thread) {
            Ok(thread) => Box::into_raw(thread).cast(),
            Err(_) => core::ptr::null_mut(),
        }
    }

    unsafe fn thread_free(thread: *mut ()) {
        // SAFETY: the caller passes a state from `thread_new`, a boxed
        // `ArchThread`, which nothing uses any more.
        drop(unsafe { Box::from_raw(thread.cast::<ArchThread>()) });
    }

    unsafe fn thread_saved_context(thread: *mut ()) -> *mut u64 {
        // SAFETY: the caller passes a live state from `thread_new`.
        unsafe { (*thread.cast::<ArchThread>()).saved_rsp() }
    }

    unsafe fn thread_set_user_start(thread: *mut (), entry: u64, stack: u64, arg1: u64, arg2: u64) {
        // SAFETY: as for `thread_saved_context`.
        unsafe { (*thread.cast::<ArchThread>()).set_start([entry, stack, arg1, arg2]) };
    }

    unsafe fn thread_switch_out(thread: *mut (), user: bool) {
        // SAFETY: as for `thread_saved_context`.
        unsafe { (*thread.cast::<ArchThread>()).switch_out(user) };
    }

    unsafe fn thread_switch_in(thread: *mut (), stack_top: u64, user: bool) {
        // SAFETY: as for `thread_saved_context`.
        unsafe { (*thread.cast::<ArchThread>()).switch_in(stack_top, user) };
    }

    fn io_ports_switch(ranges: &[(u64, u64)]) {
        let mut open = percpu::this().open_ports.lock();
        if *open == ranges {
            return;
        }
        for &(start, end) in open.iter() {
            crate::gdt::set_ports(start, end, false);
        }
        open.clear();
        // `enable_ioports` made room for every range when it was granted,
        // so pushing them does not allocate.
        debug_assert!(open.capacity() >= ranges.len());
        for &(start, end) in ranges {
            crate::gdt::set_ports(start, end, true);
            open.push((start, end));
        }
    }

    fn io_ports_reserve(count: usize) -> bool {
        // The boot processor, whose list this is, is the only one.
        let mut open = percpu::this().open_ports.lock();
        let missing = count.saturating_sub(open.len());
        open.try_reserve(missing).is_ok()
    }

    fn elf_machine() -> u16 {
        kernel_core::elf::EM_X86_64
    }

    fn clock_page_header(out: &mut [u8]) -> usize {
        let header = time::clock_page_header();
        out[..header.len()].copy_from_slice(&header);
        header.len()
    }

    fn thread_stay_suspended(thread: &Arc<kernel_core::sched::Thread>) -> bool {
        crate::syscall::stay_suspended(thread)
    }

    fn user_thread_entry() -> u64 {
        crate::thread_arch::user_entry as *const () as u64
    }

    fn extended_state_layout() -> (usize, usize) {
        let layout = context::area_layout();
        (layout.size(), layout.align())
    }

    unsafe fn extended_state_init(area: *mut u8) {
        // SAFETY: the caller guarantees a zeroed area of the layout.
        unsafe { context::init_area(area) };
    }

    unsafe fn extended_state_save(area: *mut u8) {
        // SAFETY: the caller guarantees an area of the layout.
        unsafe { context::save_area(area) };
    }

    unsafe fn extended_state_restore(area: *const u8) {
        // SAFETY: the caller guarantees the area holds valid state.
        unsafe { context::restore_area(area) };
    }

    fn user_range() -> (u64, u64) {
        (crate::page_tables::USER_START, crate::page_tables::USER_END)
    }

    fn page_table_new(
        alloc: &mut dyn FnMut() -> Option<u64>,
    ) -> Result<*mut (), kernel_core::arch::PageError> {
        let tables = SpaceTables::new(alloc)?;
        match kernel_core::fallible::try_box(tables) {
            Ok(tables) => Ok(Box::into_raw(tables).cast()),
            Err(_) => Err(kernel_core::arch::PageError::OutOfMemory),
        }
    }

    unsafe fn page_table_map(
        table: *mut (),
        virt: u64,
        phys: u64,
        flags: kernel_core::arch::PageFlags,
        alloc: &mut dyn FnMut() -> Option<u64>,
    ) -> Result<(), kernel_core::arch::PageError> {
        // SAFETY: the caller passes a live state from `page_table_new`.
        unsafe { (*table.cast::<SpaceTables>()).map(virt, phys, flags, alloc) }
    }

    unsafe fn page_table_unmap(
        table: *mut (),
        virt: u64,
        size: u64,
        limit: usize,
        unmapped: &mut dyn FnMut(u64),
    ) -> Result<Option<u64>, kernel_core::arch::PageError> {
        // SAFETY: as for `page_table_map`.
        unsafe { (*table.cast::<SpaceTables>()).unmap(virt, size, limit, unmapped) }
    }

    unsafe fn page_table_activate(table: *mut ()) {
        // SAFETY: as for `page_table_map`.
        unsafe { (*table.cast::<SpaceTables>()).activate() };
    }

    fn page_table_activate_kernel() {
        crate::page_tables::activate_kernel();
    }

    unsafe fn page_table_free(table: *mut (), free: &mut dyn FnMut(u64)) {
        // SAFETY: the caller passes a state from `page_table_new`, a boxed
        // `SpaceTables`, which nothing uses any more.
        let tables = unsafe { Box::from_raw(table.cast::<SpaceTables>()) };
        tables.free(free);
    }

    fn user_copy_in(dst: &mut [u8], src: u64) -> Result<(), kernel_core::status::Error> {
        crate::usercopy::copy_in(dst, src)
    }

    unsafe fn enter_user(entry: u64, stack: u64, arg1: u64, arg2: u64) -> ! {
        // SAFETY: the caller guarantees the thread's user state is in
        // place.
        unsafe { context::enter_user(entry, stack, arg1, arg2) }
    }
}

kernel_core::arch_impl!(X86);
