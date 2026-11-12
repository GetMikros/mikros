//! The interface between the kernel core and an architecture layer.
//!
//! The operations are listed once, in the `interface!` invocation below,
//! which turns the list into:
//!
//! - the `Arch` trait, which an architecture layer implements;
//! - a safe or `unsafe` function of the same name in this module for
//!   each operation, which the core calls;
//! - the `arch_impl!` macro, which an architecture layer invokes with its
//!   implementing type to define the symbols those functions call.
//!
//! The core cannot depend on an architecture crate, because that crate
//! depends on the core, so the functions reach the implementation by
//! symbol name at link time. Generating both sides from one list keeps
//! their signatures the same, which the linker does not check.
//!
//! On the development host the core defines the symbols itself with the
//! mock in `mock`, so that host tests and fuzz targets link.

#[cfg(not(target_os = "none"))]
pub mod mock;
#[cfg(test)]
mod tests;

macro_rules! interface {
    ($(
        $(#[$meta:meta])*
        [$($qualifier:tt)*] fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?;
    )*) => {
        /// The operations an architecture layer provides to the core.
        /// Each is documented at its function in `kernel_core::arch`.
        pub trait Arch {
            $(
                $(#[$meta])*
                $($qualifier)* fn $name($($arg: $ty),*) $(-> $ret)?;
            )*
        }

        /// The symbols that `arch_impl!` defines.
        mod symbols {
            #[allow(unused_imports)]
            use super::*;

            unsafe extern "Rust" {
                $(
                    #[link_name = concat!("__mikros_arch_", stringify!($name))]
                    pub(super) fn $name($($arg: $ty),*) $(-> $ret)?;
                )*
            }
        }

        $(
            $(#[$meta])*
            #[inline]
            pub $($qualifier)* fn $name($($arg: $ty),*) $(-> $ret)? {
                // SAFETY: the symbol is defined by `arch_impl!` with this
                // signature. An `unsafe` operation passes its contract on to
                // its caller.
                unsafe { symbols::$name($($arg),*) }
            }
        )*

        /// Defines the symbols of the architecture interface with the
        /// operations of `$ty`, which implements `kernel_core::arch::Arch`.
        /// Invoke it once in the kernel.
        #[macro_export]
        macro_rules! arch_impl {
            ($$ty:ty) => {
                $(
                    const _: () = {
                        #[unsafe(export_name = concat!("__mikros_arch_", stringify!($name)))]
                        #[allow(unused_unsafe)]
                        $($qualifier)* extern "Rust" fn shim($($arg: $ty),*) $(-> $ret)? {
                            // SAFETY: the core calls this with the operation's
                            // contract upheld.
                            unsafe { <$$ty as $crate::arch::Arch>::$name($($arg),*) }
                        }
                    };
                )*
            };
        }
    };
}

interface! {
    /// Stops this processor for good, with interrupts disabled.
    [] fn halt() -> !;

    /// Waits with interrupts enabled until an interrupt has been handled,
    /// and returns with them disabled again. Interrupts must be disabled
    /// when it is called.
    [] fn wait_for_interrupt();

    /// Whether interrupts are enabled on this processor.
    [] fn interrupts_enabled() -> bool;

    /// Disables interrupts on this processor and returns whether they were
    /// enabled.
    [] fn interrupts_disable() -> bool;

    /// Enables interrupts on this processor if `enabled`, as returned by
    /// `interrupts_disable`.
    ///
    /// # Safety
    ///
    /// Nothing that runs with interrupts disabled may still need them
    /// disabled, such as a held lock that interrupt handlers also take.
    [unsafe] fn interrupts_restore(enabled: bool);

    /// Whether this processor is handling an interrupt or exception, in
    /// which it may neither enable interrupts nor switch threads.
    [] fn handling_interrupt() -> bool;

    /// The number of this processor, from 0 for the boot processor.
    [] fn cpu_index() -> u32;

    /// The core's data for this processor, as last set by `set_cpu_data`,
    /// or null.
    [] fn cpu_data() -> *const ();

    /// Sets the core's data for this processor.
    ///
    /// # Safety
    ///
    /// `data` must stay valid for as long as the processor runs, and the
    /// core must only read it as the type it stored.
    [unsafe] fn set_cpu_data(data: *const ());

    /// The monotonic clock, in nanoseconds since boot.
    [] fn monotonic_now() -> i64;

    /// How often, in nanoseconds, `clock_maintain` must run for the
    /// monotonic clock to stay correct, or `None` if it never needs to.
    [] fn clock_maintenance_interval() -> Option<i64>;

    /// Does the clock's periodic upkeep, such as extending a counter that
    /// wraps. Called at least as often as `clock_maintenance_interval`
    /// gives.
    [] fn clock_maintain();

    /// A counter of processor cycles, for measuring timing jitter.
    [] fn cycle_counter() -> u64;

    /// Whether the processor has a random source for `random_seed`.
    [] fn random_source_present() -> bool;

    /// Fills the start of `out` with output of the processor's random
    /// source and returns how many bytes it filled, 0 when there is no
    /// source or it failed. `out` holds at least 64 bytes.
    [] fn random_seed(out: &mut [u8]) -> usize;

    /// Arms this processor's timer to interrupt at the monotonic time
    /// `deadline`, replacing any earlier setting. A deadline that has
    /// passed interrupts at once.
    [] fn timer_arm(deadline: i64);

    /// Stops this processor's timer.
    [] fn timer_disarm();

    /// Writes the log line `line`, which ends in a line feed, to the boot
    /// framebuffer's console, if there is one and it was not released.
    [] fn console_write(line: &[u8]);

    /// The kernel address at which the RAM frame at `phys` can be read and
    /// written.
    [] fn frame_address(phys: u64) -> u64;

    /// Maps the page of physical memory at `phys` writable with `policy`
    /// into kernel space and returns its address, or `None` when address
    /// space or page tables run out.
    [] fn physical_page_map(phys: u64, policy: ::kernel_core::arch::CachePolicy) -> Option<u64>;

    /// Removes a mapping from `physical_page_map`.
    ///
    /// # Safety
    ///
    /// `virt` must come from `physical_page_map`, and nothing may use the
    /// mapping any more.
    [unsafe] fn physical_page_unmap(virt: u64);

    /// Maps the RAM frames `frames` writable and contiguous into kernel
    /// space and returns the address of the first, or `None` when address
    /// space or page tables run out.
    [] fn kernel_frames_map(frames: &[u64]) -> Option<u64>;

    /// Removes a mapping of `len` bytes from `kernel_frames_map`.
    ///
    /// # Safety
    ///
    /// `virt` and `len` must describe a whole mapping from
    /// `kernel_frames_map`, and nothing may use it any more.
    [unsafe] fn kernel_frames_unmap(virt: u64, len: u64);

    /// Points this processor's coverage hook at the buffer at `area`, which
    /// holds `capacity` addresses after its count, or at none when `area`
    /// is zero.
    [] fn coverage_point(area: u64, capacity: u64);

    /// Interrupts processor `cpu`, so that it looks at its run queue and
    /// its reschedule flag again.
    [] fn reschedule_send(cpu: usize);

    /// Called on each turn of a spin loop, so that the processor handles
    /// requests from other processors that it would otherwise take as
    /// interrupts, which kernel code runs with disabled.
    [] fn spin_wait();

    /// Allocates `size` bytes of kernel memory, a multiple of the page
    /// size, mapped writable with an unmapped page below it, and returns
    /// its start, or `None` when memory runs out.
    [] fn kernel_pages_alloc(size: u64) -> Option<u64>;

    /// Frees kernel memory from `kernel_pages_alloc`.
    ///
    /// # Safety
    ///
    /// `start` and `size` must be those of an allocation, and nothing may
    /// use its memory any more.
    [unsafe] fn kernel_pages_free(start: u64, size: u64);

    /// Writes `bytes` to the debug output.
    [] fn debug_write(bytes: &[u8]);

    /// Prepares the kernel stack whose top is `stack_top` so that
    /// switching to the returned value with `context_switch` calls the
    /// function at address `entry` with `arg`, which must never return.
    ///
    /// # Safety
    ///
    /// The 128 bytes below `stack_top` must be mapped and unused.
    [unsafe] fn context_prepare(stack_top: u64, entry: u64, arg: u64) -> u64;

    /// Saves the running kernel context in `save` and resumes the one in
    /// `load`. Returns when something switches back to `save`.
    ///
    /// # Safety
    ///
    /// `load` must come from `context_prepare` or from a `save` of an
    /// earlier switch, and its stack must be used by nothing else.
    [unsafe] fn context_switch(save: *mut u64, load: u64);

    /// Allocates the architecture's state of a new thread, or returns null
    /// when memory runs out.
    [] fn thread_new() -> *mut ();

    /// Frees the state of a thread from `thread_new`.
    ///
    /// # Safety
    ///
    /// `thread` must come from `thread_new`, and nothing may use it any
    /// more.
    [unsafe] fn thread_free(thread: *mut ());

    /// Where the thread's kernel context is saved while it does not run,
    /// for `context_switch`.
    ///
    /// # Safety
    ///
    /// `thread` must come from `thread_new` and not be freed.
    [unsafe] fn thread_saved_context(thread: *mut ()) -> *mut u64;

    /// Records where a new user thread starts in user mode: at `entry`
    /// with the stack pointer `stack` and the two arguments.
    ///
    /// # Safety
    ///
    /// As for `thread_saved_context`; the thread must not have run.
    [unsafe] fn thread_set_user_start(thread: *mut (), entry: u64, stack: u64, arg1: u64, arg2: u64);

    /// Saves the processor state of the running thread that the next one
    /// replaces, including its user state if `user`.
    ///
    /// # Safety
    ///
    /// As for `thread_saved_context`; the thread must be the running one,
    /// about to be switched away from.
    [unsafe] fn thread_switch_out(thread: *mut (), user: bool);

    /// Loads the processor state of the thread about to run, whose kernel
    /// stack ends at `stack_top`, including its user state if `user`.
    ///
    /// # Safety
    ///
    /// As for `thread_saved_context`; the thread must be about to be
    /// switched to.
    [unsafe] fn thread_switch_in(thread: *mut (), stack_top: u64, user: bool);

    /// Makes this processor allow user mode exactly the I/O port ranges
    /// `ranges`, each a start and an end past it, as a thread of a process
    /// granted them is switched in. The architecture keeps room for them
    /// from `io_ports_reserve`, so this does not allocate.
    [] fn io_ports_switch(ranges: &[(u64, u64)]);

    /// Makes room for `count` I/O port ranges in what `io_ports_switch`
    /// keeps, on every processor. Returns false when memory runs out.
    [] fn io_ports_reserve(count: usize) -> bool;

    /// Sets up the boot processor and what the kernel needs before its
    /// services start: descriptor and interrupt tables, the console, and
    /// the system call entry. Clears flags in `info` for boot resources the
    /// architecture finds unusable.
    ///
    /// # Safety
    ///
    /// Called once, on the boot processor, on the kernel's page tables and
    /// stack, with interrupts disabled.
    [unsafe] fn early_setup(info: &mut ::boot_protocol::BootInfo);

    /// Sets up the interrupt controllers, the monotonic clock, the timer
    /// and DMA isolation from the firmware's tables.
    ///
    /// # Safety
    ///
    /// Called once, on the boot processor, after `early_setup`.
    [unsafe] fn platform_setup(info: &::boot_protocol::BootInfo);

    /// Gives the memory the bootloader used to the frame allocator, once
    /// nothing reads it any more, and returns the number of free frames.
    ///
    /// # Safety
    ///
    /// Called once, after the last use of boot memory, including `info`'s
    /// memory map and command line.
    [unsafe] fn boot_memory_reclaim(info: &::boot_protocol::BootInfo) -> u64;

    /// The ELF machine number of user programs.
    [] fn elf_machine() -> u16;

    /// Writes the start of the clock page given to the initial process to
    /// `out`, a page long, and returns how many bytes it wrote.
    [] fn clock_page_header(out: &mut [u8]) -> usize;

    /// Keeps the running `thread`, which waits in a system call, suspended
    /// while it is to be suspended, with the registers it entered the call
    /// with available to the debugger. Returns whether it was suspended.
    [] fn thread_stay_suspended(thread: &::alloc::sync::Arc<::kernel_core::sched::Thread>) -> bool;

    /// The address of the function a new user thread starts with on its
    /// kernel stack, for `context_prepare`. It enters user mode where
    /// `thread_set_user_start` said.
    [] fn user_thread_entry() -> u64;

    /// The size and alignment of a thread's extended processor state.
    [] fn extended_state_layout() -> (usize, usize);

    /// Puts the initial extended state in `area`.
    ///
    /// # Safety
    ///
    /// `area` must be zeroed and of the layout `extended_state_layout`
    /// gives.
    [unsafe] fn extended_state_init(area: *mut u8);

    /// Saves this processor's extended state in `area`.
    ///
    /// # Safety
    ///
    /// `area` must be of the layout `extended_state_layout` gives.
    [unsafe] fn extended_state_save(area: *mut u8);

    /// Loads the extended state in `area` on this processor.
    ///
    /// # Safety
    ///
    /// `area` must hold initial or saved extended state.
    [unsafe] fn extended_state_restore(area: *const u8);

    /// The user address range: its lowest address and the address past
    /// its end, both multiples of the page size.
    [] fn user_range() -> (u64, u64);

    /// Makes the page tables of a new user address space, which maps
    /// nothing in the user range and shares the kernel's mappings, taking
    /// table frames from `alloc`. Returns the architecture's state of
    /// them. On failure, the frames taken are freed again.
    [] fn page_table_new(alloc: &mut dyn FnMut() -> Option<u64>) -> Result<*mut (), ::kernel_core::arch::PageError>;

    /// Maps the 4 KiB user page at `virt` to the frame at `phys` in
    /// `table`, taking the frames of tables it needs from `alloc`.
    ///
    /// # Safety
    ///
    /// `table` must come from `page_table_new` and not be freed, and the
    /// caller must keep other changes of it out meanwhile.
    [unsafe] fn page_table_map(
        table: *mut (),
        virt: u64,
        phys: u64,
        flags: ::kernel_core::arch::PageFlags,
        alloc: &mut dyn FnMut() -> Option<u64>,
    ) -> Result<(), ::kernel_core::arch::PageError>;

    /// Removes the mapped pages among the `size` bytes of user addresses
    /// from `virt` in `table`, calling `unmapped` with the frame of each,
    /// and invalidates their translations, so that no processor uses them
    /// once it returns. Visits at most `limit` entries, and returns the
    /// address it stopped at when the range goes on.
    ///
    /// # Safety
    ///
    /// As for `page_table_map`.
    [unsafe] fn page_table_unmap(
        table: *mut (),
        virt: u64,
        size: u64,
        limit: usize,
        unmapped: &mut dyn FnMut(u64),
    ) -> Result<Option<u64>, ::kernel_core::arch::PageError>;

    /// Unlinks the tables of `table` that map nothing and hold entries
    /// for the `size` bytes from `virt`, and passes each table frame to
    /// `free` once no processor can use a translation through it.
    ///
    /// # Safety
    ///
    /// As for `page_table_map`.
    [unsafe] fn page_table_trim(
        table: *mut (),
        virt: u64,
        size: u64,
        free: &mut dyn FnMut(u64),
    );

    /// Loads `table` on this processor.
    ///
    /// # Safety
    ///
    /// As for `page_table_map`; its mappings must be ones the running
    /// thread may use.
    [unsafe] fn page_table_activate(table: *mut ());

    /// Loads the kernel's own page tables on this processor.
    [] fn page_table_activate_kernel();

    /// Frees `table`, passing each of its table frames to `free`. No user
    /// page may be mapped in it any more.
    ///
    /// # Safety
    ///
    /// `table` must come from `page_table_new`, and nothing may use it any
    /// more.
    [unsafe] fn page_table_free(table: *mut (), free: &mut dyn FnMut(u64));

    /// Copies `dst.len()` bytes from the user address `src` of the running
    /// thread's address space into `dst`. Fails with `InvalidArgs` when the
    /// range is not in the user address range or a page of it cannot be
    /// read.
    [] fn user_copy_in(dst: &mut [u8], src: u64) -> Result<(), ::kernel_core::status::Error>;

    /// Leaves the kernel for user mode at `entry` with the stack pointer
    /// `stack`, passing `arg1` and `arg2` in the first two argument
    /// registers.
    ///
    /// # Safety
    ///
    /// The running thread's address space must be loaded and its
    /// extended state restored.
    [unsafe] fn enter_user(entry: u64, stack: u64, arg1: u64, arg2: u64) -> !;
}

#[cfg(not(target_os = "none"))]
arch_impl!(mock::Mock);

/// How memory a mapping reaches is cached. Every architecture maps each
/// policy to its own memory types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CachePolicy {
    Cached = 0,
    WriteCombining = 1,
    Uncached = 2,
    UncachedDevice = 3,
}

/// How a user page is mapped. Every mapping is readable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageFlags {
    pub writable: bool,
    pub executable: bool,
    pub policy: CachePolicy,
}

/// Why a change of page tables failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageError {
    /// No frame could be had for a table.
    OutOfMemory,
    /// The page is already mapped.
    AlreadyMapped,
    /// The tables do not allow the change, such as a range that ends
    /// inside a large page.
    Refused,
}

/// The page tables of one user address space. They are freed with
/// `free`, which needs a place to return table frames to; dropping them
/// otherwise leaks them.
pub struct PageTable(core::ptr::NonNull<()>);

// SAFETY: the architecture's page table state is made to be changed and
// loaded from any processor, with changes kept apart by the owner.
unsafe impl Send for PageTable {}
// SAFETY: as for `Send`.
unsafe impl Sync for PageTable {}

impl PageTable {
    /// New page tables, with table frames from `alloc`.
    pub fn new(alloc: &mut dyn FnMut() -> Option<u64>) -> Result<PageTable, PageError> {
        let table = page_table_new(alloc)?;
        core::ptr::NonNull::new(table)
            .map(PageTable)
            .ok_or(PageError::OutOfMemory)
    }

    /// Maps the user page at `virt` to `phys`. The caller keeps other
    /// changes of the tables out meanwhile.
    pub fn map(
        &self,
        virt: u64,
        phys: u64,
        flags: PageFlags,
        alloc: &mut dyn FnMut() -> Option<u64>,
    ) -> Result<(), PageError> {
        // SAFETY: the state is alive while `self` is, and the caller keeps
        // other changes out.
        unsafe { page_table_map(self.0.as_ptr(), virt, phys, flags, alloc) }
    }

    /// Removes and invalidates mapped pages, as `page_table_unmap` does.
    /// The caller keeps other changes of the tables out meanwhile.
    pub fn unmap(
        &self,
        virt: u64,
        size: u64,
        limit: usize,
        unmapped: &mut dyn FnMut(u64),
    ) -> Result<Option<u64>, PageError> {
        // SAFETY: as for `map`.
        unsafe { page_table_unmap(self.0.as_ptr(), virt, size, limit, unmapped) }
    }

    /// Frees the tables that map nothing among those for the `size` bytes
    /// from `virt`, as `page_table_trim` does. The caller keeps other
    /// changes of the tables out meanwhile.
    pub fn trim(&self, virt: u64, size: u64, free: &mut dyn FnMut(u64)) {
        // SAFETY: as for `map`.
        unsafe { page_table_trim(self.0.as_ptr(), virt, size, free) }
    }

    /// Loads the tables on this processor.
    pub fn activate(&self) {
        // SAFETY: the state is alive while `self` is; the owner loads the
        // tables only for threads that may use them.
        unsafe { page_table_activate(self.0.as_ptr()) };
    }

    /// Frees the tables, passing their table frames to `free`. No user
    /// page may be mapped any more.
    pub fn free(self, free: &mut dyn FnMut(u64)) {
        // SAFETY: `self` is consumed, so nothing uses the state any more.
        unsafe { page_table_free(self.0.as_ptr(), free) };
    }
}

/// The architecture's state of one thread, freed when it is dropped.
pub struct ThreadData(core::ptr::NonNull<()>);

// SAFETY: an architecture's thread state is shared by the processors that
// run the thread, and its operations are made for that.
unsafe impl Send for ThreadData {}
// SAFETY: as for `Send`.
unsafe impl Sync for ThreadData {}

impl ThreadData {
    /// New thread state, or `None` when memory runs out.
    pub fn new() -> Option<ThreadData> {
        core::ptr::NonNull::new(thread_new()).map(ThreadData)
    }

    /// The pointer the architecture layer made the state with.
    pub fn as_ptr(&self) -> *mut () {
        self.0.as_ptr()
    }

    /// Where the thread's kernel context is saved while it does not run.
    pub fn saved_context(&self) -> *mut u64 {
        // SAFETY: the state is not freed while `self` lives.
        unsafe { thread_saved_context(self.as_ptr()) }
    }

    /// Records where a new user thread starts in user mode. The thread
    /// must not have run.
    pub fn set_user_start(&self, entry: u64, stack: u64, arg1: u64, arg2: u64) {
        // SAFETY: as for `saved_context`.
        unsafe { thread_set_user_start(self.as_ptr(), entry, stack, arg1, arg2) };
    }

    /// Saves the state of the running thread, which is being switched
    /// away from.
    pub fn switch_out(&self, user: bool) {
        // SAFETY: as for `saved_context`.
        unsafe { thread_switch_out(self.as_ptr(), user) };
    }

    /// Loads the state of the thread being switched to, whose kernel stack
    /// ends at `stack_top`.
    pub fn switch_in(&self, stack_top: u64, user: bool) {
        // SAFETY: as for `saved_context`.
        unsafe { thread_switch_in(self.as_ptr(), stack_top, user) };
    }
}

impl Drop for ThreadData {
    fn drop(&mut self) {
        // SAFETY: the state came from `thread_new`, and nothing uses it
        // once its owner is dropped.
        unsafe { thread_free(self.as_ptr()) };
    }
}
