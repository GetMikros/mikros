//! `stat`, `lstat`, `fstat` and `fstatat`, from the attributes of a node.
//!
//! The file type comes from the node's type and the permission bits from
//! its stored mode. Each namespace entry is a device of its own, the
//! owner is the user ID, and the group ID is 0. A stream is a pipe.

use crate::errno::{self, EBADF, EINVAL, EIO};
use crate::fd::{self, Description};
use crate::fs::{self, O_NOFOLLOW};
use mikros_rt::interfaces::mikros::fs::{Attributes, NodeClient, NodeType, Rights};

const S_IFIFO: u32 = 0o010000;
const S_IFDIR: u32 = 0o040000;
const S_IFCHR: u32 = 0o020000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
const S_IFSOCK: u32 = 0o140000;
/// The permission, set-ID and sticky bits of a mode.
const PERMISSIONS: u32 = 0o7777;
pub const AT_SYMLINK_NOFOLLOW: i64 = 0x100;
const AT_NO_AUTOMOUNT: i64 = 0x800;
const AT_EMPTY_PATH: i64 = 0x1000;
/// The preferred I/O size: the page size, in which memory objects map.
const BLOCK_SIZE: i64 = 4096;
const NANOSECONDS: i64 = 1_000_000_000;
const FOREVER: i64 = i64::MAX;

/// `struct stat` as musl receives it on x86_64.
#[repr(C)]
#[derive(Default)]
pub struct Kstat {
    dev: u64,
    ino: u64,
    nlink: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    pad: u32,
    rdev: u64,
    size: i64,
    blksize: i64,
    blocks: i64,
    times: [i64; 6],
    unused: [i64; 3],
}

/// The `S_IF` file type bits of `kind`.
pub fn file_type(kind: NodeType) -> u32 {
    match kind {
        NodeType::File => S_IFREG,
        NodeType::Directory => S_IFDIR,
        NodeType::Symlink => S_IFLNK,
        NodeType::Socket => S_IFSOCK,
        NodeType::Unknown(_) => 0,
    }
}

/// The seconds and nanoseconds of a UTC time in nanoseconds.
fn split(time: i64) -> [i64; 2] {
    [time.div_euclid(NANOSECONDS), time.rem_euclid(NANOSECONDS)]
}

fn from_attributes(a: &Attributes, device: u64) -> Kstat {
    let [atime, atime_ns] = split(a.accessed);
    let [mtime, mtime_ns] = split(a.modified);
    let [ctime, ctime_ns] = split(a.changed);
    Kstat {
        dev: device,
        ino: a.inode,
        nlink: a.links,
        mode: file_type(a.r#type) | a.mode & PERMISSIONS,
        uid: a.owner,
        size: a.size as i64,
        blksize: BLOCK_SIZE,
        blocks: a.allocated.div_ceil(512) as i64,
        times: [atime, atime_ns, mtime, mtime_ns, ctime, ctime_ns],
        ..Kstat::default()
    }
}

/// The attributes of `node` as a `struct stat`.
fn attributes(node: &NodeClient, device: u64) -> Result<Kstat, i64> {
    match node.get_attributes(FOREVER) {
        Ok(Ok(r)) => Ok(from_attributes(&r.attributes, device)),
        Ok(Err(e)) => Err(errno::from_fs(e)),
        Err(_) => Err(EIO),
    }
}

fn store(out: *mut Kstat, st: Result<Kstat, i64>) -> i64 {
    if out.is_null() {
        return -errno::EFAULT;
    }
    match st {
        Ok(st) => {
            // SAFETY: musl passes a `struct stat` to fill.
            unsafe { out.write(st) };
            0
        }
        Err(e) => -e,
    }
}

pub fn fstat(fd: i64, out: *mut Kstat) -> i64 {
    let Some(description) = fd::get(fd) else {
        return -EBADF;
    };
    let st = match &*description {
        Description::File(node) | Description::Directory(node, _) => {
            attributes(&node.node.lock(), node.device)
        }
        Description::Stream(_) => Ok(Kstat {
            mode: S_IFIFO | 0o600,
            nlink: 1,
            blksize: BLOCK_SIZE,
            ..Kstat::default()
        }),
        // A character device, as a terminal is on Linux.
        Description::Terminal(_) => Ok(Kstat {
            mode: S_IFCHR | 0o620,
            nlink: 1,
            blksize: BLOCK_SIZE,
            ..Kstat::default()
        }),
        // An anonymous file, as on Linux.
        Description::Epoll(_) | Description::Timer(_) => Ok(Kstat {
            mode: 0o600,
            nlink: 1,
            blksize: BLOCK_SIZE,
            ..Kstat::default()
        }),
    };
    store(out, st)
}

/// `fstatat`, which `stat` and `lstat` are with `AT_FDCWD`.
pub fn fstatat(dirfd: i64, path: *const u8, out: *mut Kstat, flag: i64) -> i64 {
    if flag & !(AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT | AT_EMPTY_PATH) != 0 {
        return -EINVAL;
    }
    // SAFETY: musl passes a NUL-terminated path.
    if flag & AT_EMPTY_PATH != 0 && !path.is_null() && unsafe { *path } == 0 {
        return fstat(dirfd, out);
    }
    let follow = if flag & AT_SYMLINK_NOFOLLOW != 0 {
        O_NOFOLLOW
    } else {
        0
    };
    // SAFETY: musl passes a NUL-terminated path.
    let st = unsafe { fs::c_path(path) }
        .and_then(|bytes| fs::absolute(dirfd, bytes))
        // Attributes need no rights.
        .and_then(|path| fs::connect(&path, follow, 0, Some(Rights(0))))
        .and_then(|c| attributes(&c.node, c.device));
    store(out, st)
}
