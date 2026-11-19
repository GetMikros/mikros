//! What the file commands ask the node connection behind a file for
//! (MIKSPEC-0098.R33): what the caller may do (MIKSPEC-0049.R36), its
//! access control lists, the file system that holds it, and sharing
//! contents with another file.

use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::fs::{AclEntry, AclTag, Error, FilesystemInfo};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::mikros::io::AsHandleExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const FOREVER: i64 = i64::MAX;
/// The C library's flag that opens a link itself.
const O_NOFOLLOW: i32 = 0o400000;

pub use mikros_rt::interfaces::mikros::fs::{NodeClient, Rights};

/// The node connection behind `file`.
pub fn node(file: &File) -> io::Result<NodeClient> {
    let handle = file.handle()?.into_raw_handle();
    // SAFETY: the handle is a new one, which this connection now owns.
    let handle = unsafe { OwnedHandle::from_raw(handle) };
    Ok(NodeClient::new(ClientEnd::new(handle)))
}

/// A connection to `path`, or to the link itself unless `follow`, with
/// the most rights that its folder gives: read and write where it may,
/// read otherwise.
pub fn open(path: &Path, follow: bool) -> io::Result<NodeClient> {
    let flags = if follow { 0 } else { O_NOFOLLOW };
    let both = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(flags)
        .open(path);
    let file = match both {
        Ok(file) => file,
        Err(_) => OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(path)?,
    };
    node(&file)
}

/// The process's namespace entries (MIKSPEC-0060.R13), each with a
/// connection of its own to its root, in the order the process got them.
pub fn entries() -> io::Result<Vec<(PathBuf, NodeClient)>> {
    const SAME_RIGHTS: u32 = 1 << 31;
    let mut found = Vec::new();
    for entry in std::os::mikros::namespace::entries()? {
        let mut raw = 0;
        let root = entry.directory.as_raw_handle();
        // SAFETY: the out pointer is valid for the call.
        if unsafe { mikros_rt::sys::handle_duplicate(root, SAME_RIGHTS, &mut raw) } != mikros_rt::OK
        {
            continue;
        }
        // SAFETY: the kernel just made the handle for us.
        let handle = unsafe { OwnedHandle::from_raw(raw) };
        found.push((entry.path, NodeClient::new(ClientEnd::new(handle))));
    }
    Ok(found)
}

/// Writes what `node`'s file system holds back for it to its storage.
pub fn sync(node: &NodeClient) -> Result<(), Error> {
    match node.sync(FOREVER) {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::Io),
    }
}

/// What `node`'s connection may do, from `Access`.
pub fn access(node: &NodeClient) -> Result<Rights, Error> {
    match node.access(FOREVER) {
        Ok(Ok(reply)) => Ok(reply.rights),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::Io),
    }
}

/// `rights` as `rwx`, with `-` for each one missing.
pub fn rwx(rights: Rights) -> String {
    let mut text = String::with_capacity(3);
    for (right, letter) in [
        (Rights::READ, 'r'),
        (Rights::WRITE, 'w'),
        (Rights::EXECUTE, 'x'),
    ] {
        text.push(if rights.contains(right) { letter } else { '-' });
    }
    text
}

/// `node`'s access list, or with `default` a folder's default list.
pub fn acl(node: &NodeClient, default: bool) -> Result<Vec<AclEntry>, Error> {
    match node.get_acl(default, FOREVER) {
        Ok(Ok(reply)) => Ok(reply.entries),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::Io),
    }
}

/// Whether `entries`, an access list, is extended: it has entries for
/// named users or apps, or a mask (MIKSPEC-0049.R25).
pub fn extended(entries: &[AclEntry]) -> bool {
    entries
        .iter()
        .any(|e| matches!(e.tag, AclTag::User | AclTag::App | AclTag::Mask))
}

/// The file system that holds `node`.
pub fn filesystem(node: &NodeClient) -> Result<FilesystemInfo, Error> {
    match node.query_filesystem(FOREVER) {
        Ok(Ok(reply)) => Ok(reply.info),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::Io),
    }
}

/// Makes `length` bytes of `target` at `offset` share the contents of
/// `source` at `source_offset`, where the file system can.
pub fn clone_range(
    target: &NodeClient,
    source: &File,
    source_offset: u64,
    offset: u64,
    length: u64,
) -> Result<(), Error> {
    let handle = source.handle().map_err(|_| Error::Io)?.into_raw_handle();
    // SAFETY: the handle is a new one, which the call takes.
    let source = ClientEnd::new(unsafe { OwnedHandle::from_raw(handle) });
    match target.clone_range(source, source_offset, offset, length, FOREVER) {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::Io),
    }
}
