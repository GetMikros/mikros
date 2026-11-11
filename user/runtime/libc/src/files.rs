//! Creating, removing, renaming and linking files and folders, changing
//! their mode, size and times, and the statistics of their file system,
//! on the file system protocol of the namespace. A change goes to the
//! folder that holds the name, opened with the right to write it; one in
//! a folder above namespace entries fails with `EROFS`. Owners do not
//! change: the `chown` calls succeed only when they would change nothing,
//! and every group is 0. The file creation mask, which the startup
//! transfer gives and `umask` changes, clears its bits from the mode of
//! each file and folder made.

use crate::errno::{self, EBADF, EEXIST, EINVAL, EIO, ENOTDIR, EPERM, EROFS};
use crate::fd::{self, Description};
use crate::fs::{self, AT_FDCWD, O_DIRECTORY, O_NOFOLLOW};
use alloc::string::String;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::fs::{
    AttributeMask, Error, Node as NodeProtocol, NodeClient, NodeType, OpenFlags, Rights,
};

const FOREVER: i64 = i64::MAX;
/// The file creation mask until the startup transfer gives one.
const UMASK: u32 = 0o022;
pub const AT_REMOVEDIR: i64 = 0x200;
pub const AT_SYMLINK_NOFOLLOW: i64 = 0x100;
const AT_SYMLINK_FOLLOW: i64 = 0x400;
const AT_EMPTY_PATH: i64 = 0x1000;
const UTIME_NOW: i64 = 0x3fff_ffff;
const UTIME_OMIT: i64 = 0x3fff_fffe;
const PERMISSIONS: u32 = 0o7777;
const NANOSECONDS: i64 = 1_000_000_000;
/// `f_flags` of `struct statfs`: the fields are valid, and the file
/// system is read-only.
const ST_VALID: u64 = 0x20;
const ST_RDONLY: u64 = 0x1;
/// The longest name, which `f_namelen` gives.
const NAME_MAX: u64 = 255;

/// The result of a call on a node, as an error number.
fn done<T>(
    reply: Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>,
) -> Result<T, i64> {
    match reply {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(errno::from_fs(e)),
        Err(_) => Err(EIO),
    }
}

fn result(r: Result<(), i64>) -> i64 {
    r.map_or_else(|e| -e, |()| 0)
}

/// The folder that holds `path` relative to `dirfd`, opened to be
/// changed, and the name in it.
fn parent(dirfd: i64, path: *const u8) -> Result<(NodeClient, String), i64> {
    // SAFETY: musl passes a NUL-terminated path.
    let bytes = unsafe { fs::c_path(path) }?;
    let full = fs::absolute(dirfd, bytes)?;
    let at = full.rfind('/').ok_or(EINVAL)?;
    let (folder, name) = match at {
        0 => ("/", &full[1..]),
        at => (&full[..at], &full[at + 1..]),
    };
    if name.is_empty() {
        // `/` itself.
        return Err(EROFS);
    }
    if fs::folder_names(folder).is_some() {
        return Err(EROFS);
    }
    let c = fs::connect(folder, O_DIRECTORY, 0, Some(Rights::READ | Rights::WRITE))?;
    if c.kind != NodeType::Directory {
        return Err(ENOTDIR);
    }
    Ok((c.node, String::from(name)))
}

/// The node at `path` relative to `dirfd`, with `rights`, following a
/// final symbolic link unless `nofollow`. Changing attributes takes the
/// right to write.
fn node_at(dirfd: i64, path: *const u8, nofollow: bool, rights: Rights) -> Result<NodeClient, i64> {
    // SAFETY: musl passes a NUL-terminated path.
    let bytes = unsafe { fs::c_path(path) }?;
    let full = fs::absolute(dirfd, bytes)?;
    if fs::folder_names(&full).is_some() {
        return Err(EROFS);
    }
    let flags = if nofollow { O_NOFOLLOW } else { 0 };
    Ok(fs::connect(&full, flags, 0, Some(rights))?.node)
}

/// A new connection with `rights` to the node behind descriptor `fd`.
fn node_of(fd: i64, rights: Rights) -> Result<NodeClient, i64> {
    match fd::get(fd).as_deref() {
        Some(Description::File(node) | Description::Directory(node, _)) => {
            let (client, server) = endpoints::<NodeProtocol>().map_err(|_| errno::ENOMEM)?;
            done(node.node.lock().clone(server, rights, FOREVER))?;
            Ok(NodeClient::new(client))
        }
        Some(Description::Folder(_)) => Err(EROFS),
        Some(_) => Err(EINVAL),
        None => Err(EBADF),
    }
}

/// `mkdirat`: a new folder `path` with permissions `mode`.
pub fn mkdirat(dirfd: i64, path: *const u8, mode: u32) -> i64 {
    result((|| {
        // A folder above entries, or an entry's own path, exists.
        let (folder, name) = parent(dirfd, path).map_err(|e| match e {
            EROFS if exists(&name_of(dirfd, path)) => EEXIST,
            e => e,
        })?;
        let (_node, server) = endpoints::<NodeProtocol>().map_err(|_| errno::ENOMEM)?;
        let flags = OpenFlags::CREATE | OpenFlags::EXCLUSIVE | OpenFlags::DIRECTORY;
        let opened = folder.open(
            name,
            flags,
            mode & PERMISSIONS,
            Rights::READ,
            server,
            FOREVER,
        );
        done(opened).map(drop)
    })())
}

/// Whether something is at the absolute path `path`.
fn exists(path: &str) -> bool {
    fs::folder_names(path).is_some() || fs::connect(path, 0, 0, Some(Rights(0))).is_ok()
}

/// The absolute path of `path` relative to `dirfd`, or nothing.
fn name_of(dirfd: i64, path: *const u8) -> String {
    // SAFETY: musl passes a NUL-terminated path.
    unsafe { fs::c_path(path) }
        .and_then(|bytes| fs::absolute(dirfd, bytes))
        .unwrap_or_default()
}

/// `unlinkat`: removes the file `path`, or the empty folder with
/// `AT_REMOVEDIR`.
pub fn unlinkat(dirfd: i64, path: *const u8, flags: i64) -> i64 {
    if flags & !AT_REMOVEDIR != 0 {
        return -EINVAL;
    }
    result((|| {
        let (folder, name) = parent(dirfd, path)?;
        done(folder.unlink(name, flags & AT_REMOVEDIR != 0, FOREVER)).map(drop)
    })())
}

/// `renameat2`: moves `old` to `new`, replacing what `new` names. No
/// flags are supported.
pub fn renameat(olddirfd: i64, old: *const u8, newdirfd: i64, new: *const u8, flags: i64) -> i64 {
    if flags != 0 {
        return -EINVAL;
    }
    result((|| {
        let (from, name) = parent(olddirfd, old)?;
        let (to, new_name) = parent(newdirfd, new)?;
        let token = done(to.get_token(FOREVER))?.token;
        done(from.rename(name, token, new_name, FOREVER)).map(drop)
    })())
}

/// `linkat`: a new name `new` for the file `old`. A final symbolic link
/// is linked itself; `AT_SYMLINK_FOLLOW` is not supported.
pub fn linkat(olddirfd: i64, old: *const u8, newdirfd: i64, new: *const u8, flags: i64) -> i64 {
    if flags & !(AT_SYMLINK_FOLLOW | AT_EMPTY_PATH) != 0 {
        return -EINVAL;
    }
    if flags != 0 {
        return -errno::ENOTSUP;
    }
    result((|| {
        let (from, name) = parent(olddirfd, old)?;
        let (to, new_name) = parent(newdirfd, new)?;
        let token = done(to.get_token(FOREVER))?.token;
        done(from.link(name, token, new_name, FOREVER)).map(drop)
    })())
}

/// `symlinkat`: a symbolic link `path` to `target`.
pub fn symlinkat(target: *const u8, dirfd: i64, path: *const u8) -> i64 {
    result((|| {
        // SAFETY: musl passes a NUL-terminated target.
        let target = unsafe { fs::c_path(target) }?;
        let target = core::str::from_utf8(target).map_err(|_| EINVAL)?;
        let (folder, name) = parent(dirfd, path)?;
        done(folder.create_symlink(name, target.into(), FOREVER)).map(drop)
    })())
}

fn set_mode(node: &NodeClient, mode: u32) -> Result<(), i64> {
    let mask = AttributeMask::MODE;
    done(node.set_attributes(mask, mode & PERMISSIONS, 0, 0, FOREVER)).map(drop)
}

/// `fchmodat2`: the permissions of `path`.
pub fn fchmodat(dirfd: i64, path: *const u8, mode: u32, flags: i64) -> i64 {
    if flags & !AT_SYMLINK_NOFOLLOW != 0 {
        return -EINVAL;
    }
    let nofollow = flags & AT_SYMLINK_NOFOLLOW != 0;
    result(node_at(dirfd, path, nofollow, Rights::WRITE).and_then(|node| set_mode(&node, mode)))
}

/// `fchmod`: the permissions of the file of descriptor `fd`.
pub fn fchmod(fd: i64, mode: u32) -> i64 {
    result(node_of(fd, Rights::WRITE).and_then(|node| set_mode(&node, mode)))
}

/// `truncate`: the size of the file `path`.
pub fn truncate(path: *const u8, size: i64) -> i64 {
    let Ok(size) = u64::try_from(size) else {
        return -EINVAL;
    };
    result((|| {
        // SAFETY: musl passes a NUL-terminated path.
        let bytes = unsafe { fs::c_path(path) }?;
        let full = fs::absolute(AT_FDCWD, bytes)?;
        let c = fs::connect(&full, fs::O_WRONLY, 0, Some(Rights::WRITE))?;
        done(c.node.truncate(size, FOREVER)).map(drop)
    })())
}

/// `ftruncate`: the size of the file of descriptor `fd`, which must be
/// open for writing.
pub fn ftruncate(fd: i64, size: i64) -> i64 {
    let Ok(size) = u64::try_from(size) else {
        return -EINVAL;
    };
    match fd::get(fd).as_deref() {
        Some(Description::File(node)) => {
            result(done(node.node.lock().truncate(size, FOREVER)).map(drop))
        }
        Some(Description::Directory(..) | Description::Folder(_)) => -errno::EISDIR,
        Some(_) => -EINVAL,
        None => -EBADF,
    }
}

/// musl's `struct timespec`.
#[repr(C)]
pub struct Timespec {
    sec: i64,
    nsec: i64,
}

/// `utimensat`: the times of `path`, or of descriptor `dirfd` when `path`
/// is null, as `futimens` asks. Times that are null are now; `UTIME_NOW`
/// is now and `UTIME_OMIT` leaves a time as it is.
pub fn utimensat(dirfd: i64, path: *const u8, times: *const Timespec, flags: i64) -> i64 {
    if flags & !AT_SYMLINK_NOFOLLOW != 0 {
        return -EINVAL;
    }
    result((|| {
        // SAFETY: musl passes null or two timespecs.
        let given = unsafe { times.as_ref().map(|t| [t, &*times.add(1)]) };
        let mut mask = AttributeMask(0);
        let mut values = [0; 2];
        for (i, bit) in [AttributeMask::ACCESSED, AttributeMask::MODIFIED]
            .into_iter()
            .enumerate()
        {
            values[i] = match given.map(|t| (t[i].sec, t[i].nsec)) {
                // Now is known only once the time service gave the time.
                None | Some((_, UTIME_NOW)) => crate::clock::realtime()?,
                Some((_, UTIME_OMIT)) => continue,
                Some((_, nsec)) if !(0..NANOSECONDS).contains(&nsec) => return Err(EINVAL),
                Some((sec, nsec)) => sec * NANOSECONDS + nsec,
            };
            mask = mask | bit;
        }
        if mask.0 == 0 {
            return Ok(());
        }
        let nofollow = flags & AT_SYMLINK_NOFOLLOW != 0;
        let node = match path.is_null() {
            true => node_of(dirfd, Rights::WRITE)?,
            false => node_at(dirfd, path, nofollow, Rights::WRITE)?,
        };
        done(node.set_attributes(mask, 0, values[0], values[1], FOREVER)).map(drop)
    })())
}

/// The `struct statfs` of x86_64.
#[repr(C)]
pub struct Statfs {
    kind: u64,
    block_size: u64,
    blocks: u64,
    free: u64,
    available: u64,
    files: u64,
    free_files: u64,
    fsid: [i32; 2],
    name_max: u64,
    fragment_size: u64,
    flags: u64,
    spare: [u64; 4],
}

/// The magic number that Linux gives a file system of type `kind`.
fn magic(kind: &str) -> u64 {
    match kind {
        "btrfs" => 0x9123_683e,
        "tmpfs" => 0x0102_1994,
        "fat" | "vfat" => 0x4d44,
        "exfat" => 0x2011_bab0,
        _ => 0,
    }
}

fn statfs_of(node: &NodeClient, out: *mut Statfs) -> Result<(), i64> {
    if out.is_null() {
        return Err(errno::EFAULT);
    }
    let info = done(node.query_filesystem(FOREVER))?.info;
    let size = u64::from(info.block_size.max(1));
    let st = Statfs {
        kind: magic(&info.r#type),
        block_size: size,
        blocks: info.total_bytes / size,
        free: info.free_bytes / size,
        available: info.free_bytes / size,
        files: 0,
        free_files: 0,
        fsid: [0; 2],
        name_max: NAME_MAX,
        fragment_size: size,
        flags: ST_VALID | if info.read_only { ST_RDONLY } else { 0 },
        spare: [0; 4],
    };
    // SAFETY: musl passes a `struct statfs` to fill.
    unsafe { out.write(st) };
    Ok(())
}

/// `statfs`, from which musl's `statvfs` comes: the file system of
/// `path`.
pub fn statfs(path: *const u8, out: *mut Statfs) -> i64 {
    result(node_at(AT_FDCWD, path, false, Rights(0)).and_then(|node| statfs_of(&node, out)))
}

/// `fstatfs`, from which musl's `fstatvfs` comes.
pub fn fstatfs(fd: i64, out: *mut Statfs) -> i64 {
    result(node_of(fd, Rights(0)).and_then(|node| statfs_of(&node, out)))
}

/// Whether owner `uid` and group `gid` change nothing for a node owned by
/// `owner`, in group 0; -1 keeps either.
fn unchanged(owner: u32, uid: u32, gid: u32) -> bool {
    (uid == u32::MAX || uid == owner) && (gid == u32::MAX || gid == 0)
}

fn chown_node(node: &NodeClient, uid: u32, gid: u32) -> Result<(), i64> {
    let owner = done(node.get_attributes(FOREVER))?.attributes.owner;
    match unchanged(owner, uid, gid) {
        true => Ok(()),
        false => Err(EPERM),
    }
}

/// `fchownat`, and so `chown` and `lchown`.
pub fn fchownat(dirfd: i64, path: *const u8, uid: u32, gid: u32, flags: i64) -> i64 {
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
        return -EINVAL;
    }
    // SAFETY: musl passes a NUL-terminated path.
    let empty = flags & AT_EMPTY_PATH != 0 && !path.is_null() && unsafe { *path } == 0;
    if empty {
        return fchown(dirfd, uid, gid);
    }
    let nofollow = flags & AT_SYMLINK_NOFOLLOW != 0;
    result(node_at(dirfd, path, nofollow, Rights(0)).and_then(|node| chown_node(&node, uid, gid)))
}

/// `fchown`.
pub fn fchown(fd: i64, uid: u32, gid: u32) -> i64 {
    result(node_of(fd, Rights(0)).and_then(|node| chown_node(&node, uid, gid)))
}

/// The file creation mask.
static CREATION_MASK: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(UMASK);

/// Makes `mask`, from the startup transfer, the file creation mask.
pub fn start_with_mask(mask: u32) {
    CREATION_MASK.store(mask & 0o777, core::sync::atomic::Ordering::Relaxed);
}

/// `umask`: makes `mask` the file creation mask, and returns the mask it
/// had.
pub fn umask(mask: u32) -> i64 {
    i64::from(CREATION_MASK.swap(mask & 0o777, core::sync::atomic::Ordering::Relaxed))
}

/// The file creation mask, which `Spawn` and `Exec` pass on.
pub fn creation_mask() -> u32 {
    CREATION_MASK.load(core::sync::atomic::Ordering::Relaxed)
}

/// `mode` without the bits of the file creation mask, for a file or
/// folder being made.
pub fn masked(mode: u32) -> u32 {
    mode & !creation_mask()
}

/// `fsync` and `fdatasync`: has the file system write the descriptor's
/// node to its storage, and returns once it has replied.
pub fn fsync(fd: i64) -> i64 {
    result(node_of(fd, Rights(0)).and_then(|node| done(node.sync(FOREVER)).map(|_| ())))
}
