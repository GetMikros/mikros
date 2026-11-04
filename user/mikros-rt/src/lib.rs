//! The Mikros runtime for programs without the standard library: the
//! program entry point, the system call bindings, the global allocator,
//! logging and the panic handler.

#![no_std]

extern crate alloc;

pub mod heap;
pub mod log;
pub mod startup;

/// The system call bindings.
pub mod sys {
    include!(concat!(env!("OUT_DIR"), "/syscalls.rs"));
}

pub use startup::Startup;

/// The value a successful system call returns.
pub const OK: sys::Status = 0;
/// The exit code of a process whose program panicked.
pub const PANIC_EXIT_CODE: i64 = -1;

/// Defines `function`, taking a `&Startup` and returning an exit code, as
/// the program's main function.
#[macro_export]
macro_rules! main {
    ($function:path) => {
        #[unsafe(no_mangle)]
        fn __mikros_entry(bootstrap: $crate::sys::Handle, _arg2: u64) -> i64 {
            $crate::run_main(bootstrap, $function)
        }
    };
}

/// Defines `function`, taking the bootstrap handle and the second start
/// argument and returning an exit code, as the program's main function,
/// for programs that read their bootstrap channel themselves.
#[macro_export]
macro_rules! raw_main {
    ($function:path) => {
        #[unsafe(no_mangle)]
        fn __mikros_entry(bootstrap: $crate::sys::Handle, arg2: u64) -> i64 {
            $function(bootstrap, arg2)
        }
    };
}

unsafe extern "Rust" {
    fn __mikros_entry(bootstrap: sys::Handle, arg2: u64) -> i64;
}

/// Receives the startup message on `bootstrap` and runs `main` with it.
#[doc(hidden)]
pub fn run_main(bootstrap: sys::Handle, main: fn(&Startup) -> i64) -> i64 {
    match startup::receive(bootstrap) {
        Some(startup) => main(startup),
        None => PANIC_EXIT_CODE,
    }
}

// The kernel starts the first thread with the stack pointer 16-byte
// aligned, the bootstrap channel in RDI and the second argument in RSI.
// The call leaves the stack aligned as the SysV ABI expects at a call.
core::arch::global_asm!(
    ".globl _start",
    "_start:",
    "xor ebp, ebp",
    "call {start}",
    "ud2",
    start = sym start,
);

extern "sysv64" fn start(bootstrap: u64, arg2: u64) -> ! {
    // SAFETY: the program defines `__mikros_entry` with `main!` or
    // `raw_main!`.
    let code = unsafe { __mikros_entry(bootstrap as sys::Handle, arg2) };
    exit(code)
}

/// Ends the process with `code`.
pub fn exit(code: i64) -> ! {
    // SAFETY: process_exit takes no pointers.
    unsafe { sys::process_exit(code) };
    unreachable!("process_exit returned");
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log::write(log::Severity::Error, format_args!("panic: {info}"));
    exit(PANIC_EXIT_CODE)
}
