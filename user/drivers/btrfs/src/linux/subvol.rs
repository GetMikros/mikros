// SPDX-License-Identifier: GPL-2.0
//! Subvolumes: the calls of `shim/subvol.c` on nodes.

use crate::linux::node::Node;
use crate::linux::result;
use core::ffi::{c_char, c_int, c_void};

unsafe extern "C" {
    fn mikros_btrfs_subvolume_create(dir: *mut c_void, name: *const c_char, len: u32) -> i32;
    fn mikros_btrfs_subvolume_snapshot(
        source: *mut c_void,
        dir: *mut c_void,
        name: *const c_char,
        len: u32,
        read_only: bool,
    ) -> i32;
    fn mikros_btrfs_subvolume_delete(dir: *mut c_void, name: *const c_char, len: u32) -> i32;
    fn mikros_btrfs_subvolume_set_read_only(root: *mut c_void, read_only: bool) -> i32;
    fn mikros_btrfs_subvolumes(
        node: *mut c_void,
        cursor: *mut u64,
        entry: extern "C" fn(*mut c_void, *const u8, c_int, u64, bool) -> bool,
        arg: *mut c_void,
    ) -> i32;
}

fn name_len(name: &[u8]) -> Result<u32, i32> {
    u32::try_from(name.len()).map_err(|_| -36)
}

/// What `Node::subvolumes()` hands its callback.
type SubvolumeFn<'f> = &'f mut dyn FnMut(&[u8], u64, bool) -> bool;

extern "C" fn subvolume(arg: *mut c_void, path: *const u8, len: c_int, id: u64, ro: bool) -> bool {
    // SAFETY: `subvolumes()` passes a pointer to its callback, and the
    // shim the subvolume's path, both for the call.
    unsafe {
        let callback = &mut *arg.cast::<SubvolumeFn<'_>>();
        callback(core::slice::from_raw_parts(path, len as usize), id, ro)
    }
}

impl Node<'_> {
    /// Makes the subvolume `name` in this directory.
    pub fn create_subvolume(&self, name: &[u8]) -> Result<(), i32> {
        let len = name_len(name)?;
        // SAFETY: the node is alive, and the name is `len` bytes.
        result(unsafe { mikros_btrfs_subvolume_create(self.node, name.as_ptr().cast(), len) })
    }

    /// Makes `name` in this directory a snapshot of the subvolume whose
    /// root `source` is, read-only if `read_only`.
    pub fn snapshot(&self, source: &Node<'_>, name: &[u8], read_only: bool) -> Result<(), i32> {
        let len = name_len(name)?;
        // SAFETY: both nodes are alive, and the name is `len` bytes.
        result(unsafe {
            mikros_btrfs_subvolume_snapshot(
                source.node,
                self.node,
                name.as_ptr().cast(),
                len,
                read_only,
            )
        })
    }

    /// Removes the subvolume `name` in this directory.
    pub fn delete_subvolume(&self, name: &[u8]) -> Result<(), i32> {
        let len = name_len(name)?;
        // SAFETY: the node is alive, and the name is `len` bytes.
        result(unsafe { mikros_btrfs_subvolume_delete(self.node, name.as_ptr().cast(), len) })
    }

    /// Makes the subvolume whose root this node is read-only or writable.
    /// Fails with `EINVAL` for a node that is no subvolume's root.
    pub fn set_read_only(&self, read_only: bool) -> Result<(), i32> {
        // SAFETY: the node is alive.
        result(unsafe { mikros_btrfs_subvolume_set_read_only(self.node, read_only) })
    }

    /// Calls `entry_fn` with the path from the top-level subvolume, the ID
    /// and whether it is read-only of each subvolume of the file system
    /// with an ID above `cursor`, in order of IDs, until it returns false,
    /// which leaves that subvolume for the next call. `cursor` becomes the
    /// ID of the last one it took. Subvolumes being removed are left out.
    pub fn subvolumes(
        &self,
        cursor: &mut u64,
        mut entry_fn: impl FnMut(&[u8], u64, bool) -> bool,
    ) -> Result<(), i32> {
        let mut callback: SubvolumeFn<'_> = &mut entry_fn;
        // SAFETY: the node is alive; the shim calls `subvolume` with the
        // callback only during the call.
        result(unsafe {
            mikros_btrfs_subvolumes(
                self.node,
                cursor,
                subvolume,
                (&mut callback as *mut SubvolumeFn<'_>).cast(),
            )
        })
    }
}
