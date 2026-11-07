//! The Mikros runtime for programs without the standard library: the
//! program entry point, the system call bindings, the global allocator,
//! logging and the panic handler.

#![no_std]

extern crate alloc;

pub mod exec;
pub mod files;
pub mod heap;
pub mod log;
pub mod managed;
pub mod midl;
mod relocate;
pub mod startup;

/// The system call bindings.
pub mod sys {
    include!(concat!(env!("OUT_DIR"), "/syscalls.rs"));
}

/// The bindings of the system libraries in `midl/`.
#[allow(clippy::all, unused)]
pub mod interfaces {
    include!(concat!(env!("OUT_DIR"), "/interfaces.rs"));
}

pub use managed::Managed;
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

/// Defines `function`, taking a `&Managed` and returning an exit code, as
/// the main function of a program that the process manager starts.
#[macro_export]
macro_rules! managed_main {
    ($function:path) => {
        #[unsafe(no_mangle)]
        fn __mikros_entry(bootstrap: $crate::sys::Handle, _arg2: u64) -> i64 {
            $crate::run_managed(bootstrap, $function)
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

#[cfg(feature = "entry")]
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

/// Receives the process manager's startup transfer on `bootstrap` and
/// runs `main` with it. A refused transfer ends the program without
/// running `main`.
#[doc(hidden)]
pub fn run_managed(bootstrap: sys::Handle, main: fn(&Managed) -> i64) -> i64 {
    match managed::receive(bootstrap) {
        Ok(managed) => main(&managed),
        Err(_) => PANIC_EXIT_CODE,
    }
}

// The ABI levels of the program, the minimum it needs and the one it was
// built for, as a note of the name `Mikros` and the type 1, as the C
// library's crti.o gives C programs.
#[cfg(feature = "entry")]
core::arch::global_asm!(
    ".pushsection .note.mikros.abi, \"a\", @note",
    ".p2align 2",
    ".long 7",
    ".long 8",
    ".long 1",
    ".asciz \"Mikros\"",
    ".p2align 2",
    ".long 0",
    ".long 0",
    ".popsection",
);

// The kernel starts the first thread with the stack pointer 16-byte
// aligned, the bootstrap channel in RDI and the second argument in RSI.
// The calls leave the stack aligned as the SysV ABI expects at a call.
// The arguments are kept in callee-saved registers while the image
// relocates itself.
#[cfg(feature = "entry")]
core::arch::global_asm!(
    ".globl _start",
    "_start:",
    "xor ebp, ebp",
    "mov r12, rdi",
    "mov r13, rsi",
    "call __mikros_relocate",
    "mov rdi, r12",
    "mov rsi, r13",
    "call {start}",
    "ud2",
    start = sym start,
);

#[cfg(feature = "entry")]
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
