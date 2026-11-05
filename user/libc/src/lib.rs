//! The Mikros layer of the C library: the program start that musl's
//! start runs after, and the operations behind musl's system call
//! layer. It is linked into libc.slib with musl.

#![no_std]

extern crate alloc;

mod abi;
mod clock;
mod dir;
mod errno;
mod fd;
mod fs;
mod futex;
mod io;
mod library;
mod lock;
mod poll;
mod process;
mod signal;
mod spawn;
mod start;
mod stat;
mod syscall;
mod thread;

/// The `mikros_<name>` function of every system call of production
/// builds, which `<mikros/syscalls.h>` declares.
mod syscalls {
    use mikros_rt::sys;

    include!(concat!(env!("OUT_DIR"), "/syscalls.rs"));
}
