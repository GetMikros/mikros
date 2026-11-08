// SPDX-License-Identifier: GPL-2.0
//! Linux's btrfs code on the btrfs shim (MIKSPEC-0053.R1), which `cargo
//! xtask btrfs` builds: the file system that the btrfs server serves.
//! The program that links it provides the shim's host layer, the
//! `mikros_host_` functions of `shim/host.h`.

#![no_std]

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod host;
#[cfg(test)]
mod tests;

use core::ffi::{CStr, c_char, c_void};

unsafe extern "C" {
    fn mikros_btrfs_start() -> i32;
    fn mikros_btrfs_add_disk(
        name: *const c_char,
        handle: *mut c_void,
        size: u64,
        block_size: u32,
        read_only: bool,
    ) -> i32;
}

fn result(ret: i32) -> Result<(), i32> {
    match ret {
        0 => Ok(()),
        errno => Err(errno),
    }
}

/// Sets the shim up on the calling thread and runs the initcalls of the
/// code built on it, btrfs's among them, once for the program. The error
/// is a negative errno.
pub fn start() -> Result<(), i32> {
    // SAFETY: the shim's entry takes no arguments.
    result(unsafe { mikros_btrfs_start() })
}

/// Adds the host's disk `handle`, of `size` bytes in blocks of
/// `block_size`, as /dev/`name`, once the shim has started. The shim
/// reads and writes it with the host layer's `mikros_host_disk_`
/// functions, and keeps it for the rest of the program.
///
/// # Safety
///
/// `handle` must stay valid for the host layer's disk functions for the
/// rest of the program.
pub unsafe fn add_disk(
    name: &CStr,
    handle: *mut c_void,
    size: u64,
    block_size: u32,
    read_only: bool,
) -> Result<(), i32> {
    // SAFETY: the name is a C string, and the caller vouches for the handle.
    result(unsafe { mikros_btrfs_add_disk(name.as_ptr(), handle, size, block_size, read_only) })
}
