//! The namespace of the process, its current directory, and opening
//! paths in them.
//!
//! An absolute path loses its `.` components and its `..` components
//! lexically, never above `/`. Its namespace entry is the one with the
//! longest path that is a prefix of it at a component boundary, and the
//! rest is opened on that entry. A relative path is resolved against the
//! current directory, and a redirect from the server as a new path.

use crate::dir::Listing;
use crate::errno::{self, EINVAL, ELOOP, ENAMETOOLONG, ENOENT, ENOTDIR};
use crate::fd::{self, Description, Node};
use crate::lock::Lock;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::AtomicU32;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::fs::{
    Error, Node as NodeProtocol, NodeClient, NodeType, OpenFlags, Rights,
};
use mikros_rt::interfaces::mikros::process::NamespaceEntry;

pub const O_ACCMODE: u32 = 0o3;
pub const O_WRONLY: u32 = 0o1;
pub const O_RDWR: u32 = 0o2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DIRECTORY: u32 = 0o200000;
pub const O_NOFOLLOW: u32 = 0o400000;
pub const O_CLOEXEC: u32 = 0o2000000;
/// The descriptor that `openat` resolves relative paths against the
/// current directory with.
pub const AT_FDCWD: i64 = -100;
/// The most redirects one path resolution follows.
const REDIRECTS_MAX: usize = 40;
/// The longest path, with its NUL.
const PATH_MAX: usize = 4096;
const FOREVER: i64 = i64::MAX;

/// A namespace entry: its absolute path and its directory.
type Entry = (String, Arc<Lock<NodeClient>>);

/// The namespace entries.
static NAMESPACE: Lock<Vec<Entry>> = Lock::new(Vec::new());
/// The absolute path of the current directory.
static CWD: Lock<String> = Lock::new(String::new());

/// Installs the namespace of the startup transfer, with `/` as the
/// current directory.
pub fn install(entries: Vec<NamespaceEntry>) {
    let mut namespace = NAMESPACE.lock();
    for entry in entries {
        if let Some(path) = normalize("/", entry.path.as_bytes()) {
            namespace.push((path, Arc::new(Lock::new(NodeClient::new(entry.directory)))));
        }
    }
    *CWD.lock() = String::from("/");
}

/// The absolute form of `path`, relative to the absolute path `base`
/// when it does not start with `/`: without `.` components, with `..`
/// components removed lexically, and without a final `/`.
pub fn normalize(base: &str, path: &[u8]) -> Option<String> {
    let path = core::str::from_utf8(path).ok()?;
    let mut parts: Vec<&str> = Vec::new();
    let start = if path.starts_with('/') { "" } else { base };
    for part in start.split('/').chain(path.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let mut out = String::new();
    for part in parts {
        out.push('/');
        out.push_str(part);
    }
    if out.is_empty() {
        out.push('/');
    }
    Some(out)
}

/// The namespace entry for the absolute, normalized `path`: its path,
/// its connection, the rest of the path below it, which is empty for the
/// entry itself, and its device number.
fn entry(path: &str) -> Option<(String, Arc<Lock<NodeClient>>, String, u64)> {
    let namespace = NAMESPACE.lock();
    let mut best: Option<(usize, &Entry)> = None;
    for (i, e) in namespace.iter().enumerate() {
        let prefix = e.0.as_str();
        let below = prefix == "/"
            || path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|r| r.starts_with('/'));
        if below && best.is_none_or(|(_, b)| b.0.len() < prefix.len()) {
            best = Some((i, e));
        }
    }
    let (i, (prefix, node)) = best?;
    let rest = path[prefix.len()..].trim_start_matches('/');
    // Each entry is a device of its own, numbered from 1.
    let device = i as u64 + 1;
    Some((prefix.clone(), node.clone(), String::from(rest), device))
}

/// The C string at `path`, as bytes without its NUL.
///
/// # Safety
///
/// `path` must point to a NUL-terminated string.
pub unsafe fn c_path<'a>(path: *const u8) -> Result<&'a [u8], i64> {
    if path.is_null() {
        return Err(errno::EFAULT);
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let bytes = unsafe { core::ffi::CStr::from_ptr(path.cast()) }.to_bytes();
    if bytes.len() >= PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    if bytes.is_empty() {
        return Err(ENOENT);
    }
    Ok(bytes)
}

/// The absolute path that `path` names relative to descriptor `dirfd`,
/// or to the current directory for `AT_FDCWD`.
pub fn absolute(dirfd: i64, path: &[u8]) -> Result<String, i64> {
    let base = if path.starts_with(b"/") || dirfd == AT_FDCWD {
        CWD.lock().clone()
    } else {
        match fd::get(dirfd).as_deref() {
            Some(Description::Directory(node, _)) => node.path.clone(),
            Some(_) => return Err(ENOTDIR),
            None => return Err(errno::EBADF),
        }
    };
    normalize(&base, path).ok_or(EINVAL)
}

/// The connection rights for the access mode of `flags`. Connections for
/// reading also ask for `EXECUTE`, for mapping libraries, and do without
/// it where the directory lacks it.
fn rights(flags: u32) -> Rights {
    match flags & O_ACCMODE {
        O_WRONLY => Rights::WRITE,
        O_RDWR => Rights::READ | Rights::WRITE | Rights::EXECUTE,
        _ => Rights::READ | Rights::EXECUTE,
    }
}

fn open_flags(flags: u32) -> OpenFlags {
    let mut out = OpenFlags(0);
    for (bit, flag) in [
        (O_CREAT, OpenFlags::CREATE),
        (O_EXCL, OpenFlags::EXCLUSIVE),
        (O_TRUNC, OpenFlags::TRUNCATE),
        (O_DIRECTORY, OpenFlags::DIRECTORY),
        (O_APPEND, OpenFlags::APPEND),
        (O_NOFOLLOW, OpenFlags::NO_FOLLOW),
    ] {
        if flags & bit != 0 {
            out = out | flag;
        }
    }
    out
}

/// A new connection to a node.
pub struct Connection {
    pub node: NodeClient,
    pub kind: NodeType,
    /// The device number of the namespace entry it was opened through.
    pub device: u64,
}

/// A new connection to the absolute, normalized `path`, with `rights`,
/// or with the rights of the access mode of `flags` for `None`.
pub fn connect(
    path: &str,
    flags: u32,
    mode: u32,
    rights: Option<Rights>,
) -> Result<Connection, i64> {
    let wanted = rights;
    let mut path = String::from(path);
    for _ in 0..=REDIRECTS_MAX {
        let (prefix, directory, rest, device) = entry(&path).ok_or(ENOENT)?;
        let mut rights = wanted.unwrap_or_else(|| self::rights(flags));
        loop {
            let (client, server) = endpoints::<NodeProtocol>().map_err(|_| errno::ENOMEM)?;
            let node = directory.lock();
            let opened = if rest.is_empty() {
                NodeClient::clone(&node, server, rights, FOREVER).map(|r| r.map(|_| None))
            } else {
                let flags = open_flags(flags);
                node.open(rest.clone(), flags, mode, rights, server, FOREVER)
                    .map(|r| r.map(|o| Some((o.r#type, o.redirect))))
            };
            drop(node);
            match opened {
                Err(_) => return Err(errno::EIO),
                Ok(Err(Error::AccessDenied))
                    if wanted.is_none() && rights.contains(Rights::EXECUTE) =>
                {
                    rights = Rights(rights.0 & !Rights::EXECUTE.0);
                }
                Ok(Err(e)) => return Err(errno::from_fs(e)),
                Ok(Ok(Some((_, Some(target))))) => {
                    // A target that is not absolute is relative to the
                    // directory that `Open` was sent to.
                    path = normalize(&prefix, target.as_bytes()).ok_or(EINVAL)?;
                    break;
                }
                Ok(Ok(Some((kind, None)))) => {
                    let node = NodeClient::new(client);
                    return Ok(Connection { node, kind, device });
                }
                Ok(Ok(None)) => {
                    // The entry itself, which `Open` does not name.
                    if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                        return Err(errno::EEXIST);
                    }
                    let node = NodeClient::new(client);
                    let kind = NodeType::Directory;
                    return Ok(Connection { node, kind, device });
                }
            }
        }
    }
    Err(ELOOP)
}

/// `openat`: opens `path` relative to `dirfd` and installs a descriptor
/// for it.
pub fn open(dirfd: i64, path: *const u8, flags: u32, mode: u32) -> i64 {
    // SAFETY: musl passes a NUL-terminated path.
    let bytes = match unsafe { c_path(path) } {
        Ok(bytes) => bytes,
        Err(e) => return -e,
    };
    let path = match absolute(dirfd, bytes) {
        Ok(path) => path,
        Err(e) => return -e,
    };
    let Connection { node, kind, device } = match connect(&path, flags, mode, None) {
        Ok(opened) => opened,
        Err(e) => return -e,
    };
    let node = Node {
        node: Lock::new(node),
        path,
        device,
        flags: AtomicU32::new(flags & !(O_CREAT | O_EXCL | O_TRUNC | O_CLOEXEC)),
    };
    let description = match kind {
        NodeType::Directory => Description::Directory(node, Lock::new(Listing::default())),
        _ if flags & O_DIRECTORY != 0 => return -ENOTDIR,
        _ => Description::File(node),
    };
    fd::insert(Arc::new(description), flags & O_CLOEXEC != 0, 0)
}

/// `getcwd`: copies the current directory, with its NUL, to the `size`
/// bytes at `buf`, and returns its length with the NUL.
pub fn getcwd(buf: *mut u8, size: u64) -> i64 {
    let cwd = CWD.lock();
    let len = cwd.len() + 1;
    if buf.is_null() {
        return -errno::EFAULT;
    }
    if (size as usize) < len {
        return -errno::ERANGE;
    }
    // SAFETY: musl passes `size` bytes, which hold the path and its NUL.
    unsafe {
        core::ptr::copy_nonoverlapping(cwd.as_ptr(), buf, cwd.len());
        buf.add(cwd.len()).write(0);
    }
    len as i64
}

/// `chdir`: makes `path` the current directory, if it is a directory.
pub fn chdir(path: *const u8) -> i64 {
    // SAFETY: musl passes a NUL-terminated path.
    let path = match unsafe { c_path(path) }.and_then(|p| absolute(AT_FDCWD, p)) {
        Ok(path) => path,
        Err(e) => return -e,
    };
    match connect(&path, O_DIRECTORY, 0, Some(Rights(0))) {
        Ok(c) if c.kind == NodeType::Directory => {
            *CWD.lock() = path;
            0
        }
        Ok(_) => -ENOTDIR,
        Err(e) => -e,
    }
}

/// `fchdir`: makes the directory of descriptor `fd` the current one.
pub fn fchdir(fd: i64) -> i64 {
    match fd::get(fd).as_deref() {
        Some(Description::Directory(node, _)) => {
            *CWD.lock() = node.path.clone();
            0
        }
        Some(_) => -ENOTDIR,
        None => -errno::EBADF,
    }
}

/// `readlinkat`: copies the target of the symbolic link `path` to the
/// `size` bytes at `buf`, without a NUL and cut to fit, and returns the
/// length copied.
pub fn readlink(dirfd: i64, path: *const u8, buf: *mut u8, size: u64) -> i64 {
    if size == 0 || size > i64::MAX as u64 {
        return -EINVAL;
    }
    // SAFETY: musl passes a NUL-terminated path.
    let path = match unsafe { c_path(path) }.and_then(|p| absolute(dirfd, p)) {
        Ok(path) => path,
        Err(e) => return -e,
    };
    let link = match connect(&path, O_NOFOLLOW, 0, Some(Rights::READ)) {
        Ok(c) => c,
        Err(e) => return -e,
    };
    let target = match link.node.read_link(FOREVER) {
        Ok(Ok(r)) => r.target,
        Ok(Err(e)) => return -errno::from_fs(e),
        Err(_) => return -errno::EIO,
    };
    let len = target.len().min(size as usize);
    // SAFETY: musl passes `size` bytes, and `len` is at most that.
    unsafe { core::ptr::copy_nonoverlapping(target.as_ptr(), buf, len) };
    len as i64
}
