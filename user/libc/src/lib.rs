//! The Mikros layer of the C library: the program start that musl's
//! start runs after, and the operations behind musl's system call
//! layer. It is linked into libc.slib with musl.

#![no_std]

extern crate alloc;

mod errno;
mod fd;
mod start;
mod syscall;
