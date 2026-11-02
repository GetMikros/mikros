//! The Mikros runtime for programs without the standard library: the
//! program entry point, the system call bindings, logging and the panic
//! handler.

#![no_std]

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
        fn __mikros_main(startup: &$crate::Startup) -> i64 {
            $function(startup)
        }
    };
}

unsafe extern "Rust" {
    fn __mikros_main(startup: &Startup) -> i64;
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

extern "sysv64" fn start(bootstrap: u64, _arg2: u64) -> ! {
    let code = match startup::receive(bootstrap as sys::Handle) {
        // SAFETY: the program defines `__mikros_main` with `main!`.
        Some(startup) => unsafe { __mikros_main(startup) },
        None => PANIC_EXIT_CODE,
    };
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
