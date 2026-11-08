// SPDX-License-Identifier: GPL-2.0
//! The nodes of a mounted btrfs, which the program walks one name at a
//! time: the calls of `shim/file.c`.

use crate::{Mount, result};
use core::ffi::{c_char, c_int, c_long, c_void};
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
    fn mikros_btrfs_read_link(node: *mut c_void, buf: *mut u8, size: c_int) -> i32;
    fn mikros_btrfs_read_dir(
        dir: *mut c_void,
        cookie: *mut u64,
        entry: extern "C" fn(*mut c_void, *const u8, c_int, u64, u32) -> bool,
        arg: *mut c_void,
    ) -> i32;
    fn mikros_btrfs_create(
        dir: *mut c_void,
        name: *const c_char,
        len: u32,
        kind: c_int,
        mode: u32,
        target: *const c_char,
        node: *mut *mut c_void,
    ) -> i32;
    fn mikros_btrfs_remove(dir: *mut c_void, name: *const c_char, len: u32, directory: bool)
    -> i32;
    fn mikros_btrfs_rename(
        dir: *mut c_void,
        name: *const c_char,
        len: u32,
        new_dir: *mut c_void,
        new_name: *const c_char,
        new_len: u32,
        flags: u32,
    ) -> i32;
    fn mikros_btrfs_link(
        node: *mut c_void,
        new_dir: *mut c_void,
        new_name: *const c_char,
        new_len: u32,
    ) -> i32;
    fn mikros_btrfs_write(node: *mut c_void, offset: u64, buf: *const u8, count: usize) -> c_long;
    fn mikros_btrfs_append(
        node: *mut c_void,
        buf: *const u8,
        count: usize,
        offset: *mut u64,
    ) -> c_long;
    fn mikros_btrfs_truncate(node: *mut c_void, size: u64) -> i32;
    fn mikros_btrfs_set_attributes(
        node: *mut c_void,
        mask: u32,
        mode: u32,
        accessed: *const Time,
        modified: *const Time,
        owner: u32,
        group: u32,
    ) -> i32;
    fn mikros_btrfs_sync(node: *mut c_void) -> i32;
}

/// A time, in seconds and nanoseconds since 1970 in UTC.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Time {
    pub seconds: i64,
    pub nanoseconds: u32,
    unused: u32,
}

impl Time {
    pub const fn new(seconds: i64, nanoseconds: u32) -> Self {
        Time {
            seconds,
            nanoseconds,
            unused: 0,
        }
    }
}

/// The attributes that `Node::set_attributes()` sets, with their values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// The permission bits.
    pub mode: Option<u32>,
    pub accessed: Option<Time>,
    pub modified: Option<Time>,
    pub owner: Option<u32>,
    pub group: Option<u32>,
}

/// Flags of `Node::rename()`, as Linux's `RENAME_` flags.
pub const RENAME_NOREPLACE: u32 = 1;
pub const RENAME_EXCHANGE: u32 = 2;

/// The attributes of a node.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attributes {
    /// The type and permission bits, as Linux's `st_mode`.
    pub mode: u32,
    pub owner: u32,
    pub group: u32,
    /// The device of the node's file system, which each subvolume has
    /// one of its own of: with `inode`, what tells nodes apart.
    pub device: u32,
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

/// The types of directory entries, as Linux's `DT_` values.
pub const ENTRY_DIRECTORY: u32 = 4;
pub const ENTRY_FILE: u32 = 8;
pub const ENTRY_SYMLINK: u32 = 10;

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

/// What `Node::read_dir()` hands its callback.
type EntryFn<'f> = &'f mut dyn FnMut(&[u8], u64, u32) -> bool;

extern "C" fn entry(arg: *mut c_void, name: *const u8, len: c_int, inode: u64, kind: u32) -> bool {
    // SAFETY: `read_dir()` passes a pointer to its callback, and the shim
    // the entry's name, both for the call.
    unsafe {
        let callback = &mut *arg.cast::<EntryFn<'_>>();
        callback(core::slice::from_raw_parts(name, len as usize), inode, kind)
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

    /// Copies the target of a symbolic link into `buf`: its length.
    pub fn read_link(&self, buf: &mut [u8]) -> Result<usize, i32> {
        let size = c_int::try_from(buf.len()).unwrap_or(c_int::MAX);
        // SAFETY: as for `read()`.
        let len = unsafe { mikros_btrfs_read_link(self.node, buf.as_mut_ptr(), size) };
        usize::try_from(len).map_err(|_| len)
    }

    /// Lists the directory from position `cookie`, 0 at its start, "."
    /// and ".." too, calling `entry(name, inode, type)` with an
    /// `ENTRY_` type for each entry until it returns false, which leaves
    /// that entry for the next call, or the directory ends. `cookie`
    /// becomes the position to go on from.
    pub fn read_dir(
        &self,
        cookie: &mut u64,
        mut entry_fn: impl FnMut(&[u8], u64, u32) -> bool,
    ) -> Result<(), i32> {
        let mut callback: EntryFn<'_> = &mut entry_fn;
        // SAFETY: as for `lookup()`; the shim calls `entry` with the
        // callback only during the call.
        result(unsafe {
            mikros_btrfs_read_dir(
                self.node,
                cookie,
                entry,
                (&mut callback as *mut EntryFn<'_>).cast(),
            )
        })
    }
}

/// What `Node::create()` makes.
#[derive(Clone, Copy, Debug)]
pub enum Kind<'t> {
    File,
    Directory,
    /// A symbolic link to the target.
    Symlink(&'t core::ffi::CStr),
}

fn name_len(name: &[u8]) -> Result<u32, i32> {
    u32::try_from(name.len()).map_err(|_| -36)
}

impl<'m> Node<'m> {
    /// Makes `name` in this directory, with the permission bits `mode`
    /// for a file or a directory.
    pub fn create(&self, name: &[u8], kind: Kind<'_>, mode: u32) -> Result<Node<'m>, i32> {
        let (kind, target) = match kind {
            Kind::File => (1, core::ptr::null()),
            Kind::Directory => (2, core::ptr::null()),
            Kind::Symlink(target) => (3, target.as_ptr()),
        };
        let mut node = core::ptr::null_mut();
        // SAFETY: the node is alive, and the pointers are valid for the
        // call.
        result(unsafe {
            mikros_btrfs_create(
                self.node,
                name.as_ptr().cast(),
                name_len(name)?,
                kind,
                mode,
                target,
                &mut node,
            )
        })?;
        Ok(Node {
            node,
            mount: PhantomData,
        })
    }

    /// Removes `name` from this directory: an empty directory if
    /// `directory`, anything else otherwise.
    pub fn remove(&self, name: &[u8], directory: bool) -> Result<(), i32> {
        // SAFETY: as for `create()`.
        result(unsafe {
            mikros_btrfs_remove(self.node, name.as_ptr().cast(), name_len(name)?, directory)
        })
    }

    /// Moves `name` of this directory to `new_name` in `new_dir`, with
    /// `RENAME_` flags.
    pub fn rename(
        &self,
        name: &[u8],
        new_dir: &Node<'m>,
        new_name: &[u8],
        flags: u32,
    ) -> Result<(), i32> {
        // SAFETY: as for `create()`.
        result(unsafe {
            mikros_btrfs_rename(
                self.node,
                name.as_ptr().cast(),
                name_len(name)?,
                new_dir.node,
                new_name.as_ptr().cast(),
                name_len(new_name)?,
                flags,
            )
        })
    }

    /// Names this node, which must not be a directory, `new_name` in
    /// `new_dir` too.
    pub fn link(&self, new_dir: &Node<'m>, new_name: &[u8]) -> Result<(), i32> {
        // SAFETY: as for `create()`.
        result(unsafe {
            mikros_btrfs_link(
                self.node,
                new_dir.node,
                new_name.as_ptr().cast(),
                name_len(new_name)?,
            )
        })
    }

    /// Writes `data` to a file at `offset`: how many bytes it wrote.
    pub fn write(&self, offset: u64, data: &[u8]) -> Result<usize, i32> {
        // SAFETY: as for `create()`; the shim reads at most `data.len()`
        // bytes.
        let written = unsafe { mikros_btrfs_write(self.node, offset, data.as_ptr(), data.len()) };
        usize::try_from(written).map_err(|_| written as i32)
    }

    /// Writes `data` at the end of a file: how many bytes it wrote, and
    /// where.
    pub fn append(&self, data: &[u8]) -> Result<(usize, u64), i32> {
        let mut offset = 0;
        // SAFETY: as for `write()`.
        let written =
            unsafe { mikros_btrfs_append(self.node, data.as_ptr(), data.len(), &mut offset) };
        let written = usize::try_from(written).map_err(|_| written as i32)?;
        Ok((written, offset))
    }

    pub fn truncate(&self, size: u64) -> Result<(), i32> {
        // SAFETY: as for `create()`.
        result(unsafe { mikros_btrfs_truncate(self.node, size) })
    }

    /// Sets the attributes that `changes` holds; the change time becomes
    /// the present.
    pub fn set_attributes(&self, changes: &Changes) -> Result<(), i32> {
        let mut mask = 0;
        let mut set = |bit: u32, present: bool| {
            if present {
                mask |= bit;
            }
        };
        set(0x1, changes.mode.is_some());
        set(0x2, changes.accessed.is_some());
        set(0x4, changes.modified.is_some());
        set(0x8, changes.owner.is_some());
        set(0x10, changes.group.is_some());
        let accessed = changes.accessed.unwrap_or_default();
        let modified = changes.modified.unwrap_or_default();
        // SAFETY: as for `create()`.
        result(unsafe {
            mikros_btrfs_set_attributes(
                self.node,
                mask,
                changes.mode.unwrap_or(0),
                &accessed,
                &modified,
                changes.owner.unwrap_or(0),
                changes.group.unwrap_or(0),
            )
        })
    }

    /// Puts the node's data and attributes, and the entries that name
    /// it, on stable storage.
    pub fn sync(&self) -> Result<(), i32> {
        // SAFETY: as for `create()`.
        result(unsafe { mikros_btrfs_sync(self.node) })
    }
}

impl Drop for Node<'_> {
    fn drop(&mut self) {
        // SAFETY: the node is alive, and nothing uses it after this.
        unsafe { mikros_btrfs_put(self.node) }
    }
}
