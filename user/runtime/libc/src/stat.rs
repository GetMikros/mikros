//! `stat`, `lstat`, `fstat`, `fstatat` and `statx`, from the attributes
//! of a node, with its creation time.
//!
//! The file type comes from the node's type and the permission bits from
//! its stored mode. Each namespace entry is a device of its own, the
//! owner is the user ID, and the group ID that of the owner's own group,
//! which is the owner's ID. A stream is a pipe.

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
    /// `st_birthtim`, in the first two of musl's three unused words
    /// (MIKSPEC-0033.R41).
    birth: [i64; 2],
    unused: [i64; 1],
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
    let [btime, btime_ns] = split(a.created);
    Kstat {
        dev: device,
        ino: a.inode,
        nlink: a.links,
        mode: file_type(a.r#type) | a.mode & PERMISSIONS,
        uid: a.owner,
        // The owner's own group, whose ID is the owner's.
        gid: a.owner,
        size: a.size as i64,
        blksize: BLOCK_SIZE,
        blocks: a.allocated.div_ceil(512) as i64,
        times: [atime, atime_ns, mtime, mtime_ns, ctime, ctime_ns],
        birth: [btime, btime_ns],
        ..Kstat::default()
    }
}

/// The attributes of `node` as a `struct stat`.
/// What a folder above namespace entries reports: a directory that only
/// user 0 owns, read and searched by all.
fn folder() -> Kstat {
    Kstat {
        mode: S_IFDIR | 0o555,
        nlink: 2,
        blksize: BLOCK_SIZE,
        ..Kstat::default()
    }
}

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
        Description::Folder(_) => Ok(folder()),
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
        // A regular file of the byte size, as a memfd is on Linux.
        Description::Memory(m) => Ok(Kstat {
            mode: S_IFREG | 0o600,
            nlink: 1,
            size: m.size() as i64,
            blksize: BLOCK_SIZE,
            blocks: (m.size().div_ceil(512)) as i64,
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
        .and_then(|path| match fs::folder_names(&path) {
            Some(_) => Ok(folder()),
            // Attributes need no rights.
            None => fs::connect(&path, follow, 0, Some(Rights(0)))
                .and_then(|c| attributes(&c.node, c.device)),
        });
    store(out, st)
}

/// A time of `struct statx`.
#[repr(C)]
#[derive(Default)]
pub struct StatxTime {
    sec: i64,
    nsec: u32,
    reserved: i32,
}

/// `struct statx` as musl and Linux define it.
#[repr(C)]
#[derive(Default)]
pub struct Statx {
    mask: u32,
    blksize: u32,
    attributes: u64,
    nlink: u32,
    uid: u32,
    gid: u32,
    mode: u16,
    spare0: u16,
    ino: u64,
    size: u64,
    blocks: u64,
    attributes_mask: u64,
    atime: StatxTime,
    btime: StatxTime,
    ctime: StatxTime,
    mtime: StatxTime,
    rdev_major: u32,
    rdev_minor: u32,
    dev_major: u32,
    dev_minor: u32,
    spare2: [u64; 14],
}

/// The basic facts that `statx` gives, and the creation time.
const STATX_BASIC_STATS: u32 = 0x7ff;
const STATX_BTIME: u32 = 0x800;

/// `statx`: the facts of `fstatat`, with the creation time
/// (MIKSPEC-0033.R41), whatever `mask` asks for.
pub fn statx(dirfd: i64, path: *const u8, flag: i64, _mask: u32, out: *mut Statx) -> i64 {
    if out.is_null() {
        return -errno::EFAULT;
    }
    // Only the flags that fstatat knows; the sync ones change nothing.
    const AT_STATX_SYNC_TYPE: i64 = 0x6000;
    let mut st = Kstat::default();
    let done = fstatat(dirfd, path, &mut st, flag & !AT_STATX_SYNC_TYPE);
    if done != 0 {
        return done;
    }
    let time = |sec: i64, nsec: i64| StatxTime {
        sec,
        nsec: nsec as u32,
        reserved: 0,
    };
    let [atime, atime_ns, mtime, mtime_ns, ctime, ctime_ns] = st.times;
    let x = Statx {
        mask: STATX_BASIC_STATS | STATX_BTIME,
        blksize: st.blksize as u32,
        nlink: st.nlink as u32,
        uid: st.uid,
        gid: st.gid,
        mode: st.mode as u16,
        ino: st.ino,
        size: st.size as u64,
        blocks: st.blocks as u64,
        atime: time(atime, atime_ns),
        btime: time(st.birth[0], st.birth[1]),
        ctime: time(ctime, ctime_ns),
        mtime: time(mtime, mtime_ns),
        rdev_major: major(st.rdev),
        rdev_minor: minor(st.rdev),
        dev_major: major(st.dev),
        dev_minor: minor(st.dev),
        ..Statx::default()
    };
    // SAFETY: musl passes a `struct statx` to fill.
    unsafe { out.write(x) };
    0
}

/// The major and minor numbers of a device number, as Linux splits it.
fn major(dev: u64) -> u32 {
    (((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff)) as u32
}

fn minor(dev: u64) -> u32 {
    ((dev & 0xff) | ((dev >> 12) & !0xff)) as u32
}
