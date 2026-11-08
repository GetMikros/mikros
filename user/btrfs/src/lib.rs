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
    fn mikros_btrfs_mount(
        source: *const c_char,
        read_only: bool,
        options: *const c_char,
        mount: *mut *mut c_void,
    ) -> i32;
    fn mikros_btrfs_unmount(mount: *mut c_void);
    fn mikros_btrfs_statfs(mount: *mut c_void, statfs: *mut Statfs) -> i32;
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

/// The statistics of a mounted file system.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Statfs {
    /// The file system's magic number.
    pub kind: u64,
    pub block_size: u64,
    pub blocks: u64,
    pub blocks_free: u64,
    /// The free blocks that anyone may use.
    pub blocks_available: u64,
    pub files: u64,
    pub files_free: u64,
    /// The longest name of a file, in bytes.
    pub name_max: u64,
}

/// A mounted btrfs, which is unmounted when it is dropped.
pub struct Mount(*mut c_void);

// SAFETY: the shim's calls on a mount may come from any thread.
unsafe impl Send for Mount {}
unsafe impl Sync for Mount {}

impl Mount {
    /// Mounts the btrfs on the disk node `source`, such as `/dev/name`
    /// of `add_disk()`, read-only if `read_only`, with the btrfs mount
    /// options `options`.
    pub fn new(source: &CStr, read_only: bool, options: Option<&CStr>) -> Result<Self, i32> {
        let mut mount = core::ptr::null_mut();
        let options = options.map_or(core::ptr::null(), CStr::as_ptr);
        // SAFETY: the strings are C strings, and the pointer is valid
        // for the call.
        result(unsafe { mikros_btrfs_mount(source.as_ptr(), read_only, options, &mut mount) })?;
        Ok(Mount(mount))
    }

    pub fn statfs(&self) -> Result<Statfs, i32> {
        let mut statfs = Statfs::default();
        // SAFETY: the mount is mounted, and the pointer is valid for the
        // call.
        result(unsafe { mikros_btrfs_statfs(self.0, &mut statfs) })?;
        Ok(statfs)
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        // SAFETY: the mount is mounted, and nothing uses it after this.
        unsafe { mikros_btrfs_unmount(self.0) }
    }
}
