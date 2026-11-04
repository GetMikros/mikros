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

    /// Arms this processor's timer to interrupt at the monotonic time
    /// `deadline`, replacing any earlier setting. A deadline that has
    /// passed interrupts at once.
    [] fn timer_arm(deadline: i64);

    /// Stops this processor's timer.
    [] fn timer_disarm();

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
