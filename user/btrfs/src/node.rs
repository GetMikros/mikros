// SPDX-License-Identifier: GPL-2.0
//! The nodes of a mounted btrfs, which the program walks one name at a
//! time: the calls of `shim/file.c`.

use crate::{Mount, result};
use core::ffi::{c_char, c_long, c_void};
use core::marker::PhantomData;

unsafe extern "C" {
    fn mikros_btrfs_root(mount: *mut c_void, node: *mut *mut c_void) -> i32;
    fn mikros_btrfs_lookup(
        dir: *mut c_void,
        name: *const c_char,
        len: u32,
        node: *mut *mut c_void,
    ) -> i32;
    fn mikros_btrfs_put(node: *mut c_void);
    fn mikros_btrfs_get_attributes(node: *mut c_void, attributes: *mut Attributes) -> i32;
    fn mikros_btrfs_read(node: *mut c_void, offset: u64, buf: *mut u8, count: usize) -> c_long;
}

/// A time, in seconds and nanoseconds since 1970 in UTC.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Time {
    pub seconds: i64,
    pub nanoseconds: u32,
    unused: u32,
}

/// The attributes of a node.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attributes {
    /// The type and permission bits, as Linux's `st_mode`.
    pub mode: u32,
    pub owner: u32,
    pub group: u32,
    unused: u32,
    pub size: u64,
    /// The bytes of the disk that the node takes.
    pub allocated: u64,
    pub links: u64,
    pub inode: u64,
    /// Zero when the file system does not know it.
    pub created: Time,
    pub modified: Time,
    pub accessed: Time,
    pub changed: Time,
}

/// The type bits of `Attributes::mode`.
pub const MODE_TYPE: u32 = 0o170000;
pub const MODE_DIRECTORY: u32 = 0o040000;
pub const MODE_FILE: u32 = 0o100000;
pub const MODE_SYMLINK: u32 = 0o120000;

/// A node of a mount, which it cannot outlive.
pub struct Node<'m> {
    node: *mut c_void,
    mount: PhantomData<&'m Mount>,
}

// SAFETY: the shim's calls on a node may come from any thread.
unsafe impl Send for Node<'_> {}
unsafe impl Sync for Node<'_> {}

impl Mount {
    /// The root directory of the mount.
    pub fn root(&self) -> Result<Node<'_>, i32> {
        let mut node = core::ptr::null_mut();
        // SAFETY: the mount is mounted, and the pointer is valid for the
        // call.
        result(unsafe { mikros_btrfs_root(self.0, &mut node) })?;
        Ok(Node {
            node,
            mount: PhantomData,
        })
    }
}

impl<'m> Node<'m> {
    /// The node that `name` names in this directory, without following
    /// it if it is a symbolic link.
    pub fn lookup(&self, name: &[u8]) -> Result<Node<'m>, i32> {
        let len = u32::try_from(name.len()).map_err(|_| -36)?;
        let mut node = core::ptr::null_mut();
        // SAFETY: the node is alive, and the pointers are valid for the
        // call.
        result(unsafe { mikros_btrfs_lookup(self.node, name.as_ptr().cast(), len, &mut node) })?;
        Ok(Node {
            node,
            mount: PhantomData,
        })
    }

    pub fn attributes(&self) -> Result<Attributes, i32> {
        let mut attributes = Attributes::default();
        // SAFETY: as for `lookup()`.
        result(unsafe { mikros_btrfs_get_attributes(self.node, &mut attributes) })?;
        Ok(attributes)
    }

    /// Reads a file from `offset` into `buf`: how many bytes it read, 0 at
    /// the end.
    pub fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        // SAFETY: as for `lookup()`; the shim writes at most `buf.len()`
        // bytes.
        let read = unsafe { mikros_btrfs_read(self.node, offset, buf.as_mut_ptr(), buf.len()) };
        usize::try_from(read).map_err(|_| read as i32)
    }
}

impl Drop for Node<'_> {
    fn drop(&mut self) {
        // SAFETY: the node is alive, and nothing uses it after this.
        unsafe { mikros_btrfs_put(self.node) }
    }
}
