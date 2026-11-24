//! `Open` (MIKSPEC-0101.R45): the files of a package, served read and
//! execute as a folder for the process manager to start a portable app
//! from (MIKSPEC-0101.R46). Folders are the package's own and those that
//! its paths imply; a file is read from the package, decompressed when it
//! is compressed; and the memory object of a file that is not compressed
//! is a clone of the package file's for the file's range, so that its
//! pages are not copied.

use super::{FOREVER, Given};
use mikros_mika::format::{Entry, Kind, Package};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::fs::{
    Attributes, DirEntry, Error, FilesystemInfo, MemoryFlags, Node, NodeAccessResponse,
    NodeCloneResponse, NodeGetAttributesResponse, NodeGetMemoryObjectResponse, NodeOpenRequest,
    NodeOpenResponse, NodeQueryFilesystemResponse, NodeReadDirectoryResponse, NodeReadLinkResponse,
    NodeReadNextResponse, NodeReadResponse, NodeRequest, NodeRestrictResponse, NodeSeekResponse,
    NodeServer, NodeSyncResponse, NodeType, OpenFlags, Rights, SeekOrigin,
};
use mikros_rt::sys;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

/// What the package's files may be read, searched and run with.
const RX: Rights = Rights(Rights::READ.0 | Rights::EXECUTE.0);
/// The most entries that one `ReadDirectory` returns, and the most bytes
/// that one `Read` does.
const ENTRIES_MAX: usize = 32;
const READ_MAX: usize = 32768;
const PAGE: u64 = 4096;
/// Handle rights of the memory objects handed out.
const DUPLICATE: u32 = 1 << 0;
const TRANSFER: u32 = 1 << 1;
const READ: u32 = 1 << 2;
const EXECUTE: u32 = 1 << 4;
const MAP: u32 = 1 << 5;
const INSPECT: u32 = 1 << 9;

/// An opened package: its index and the client's connection to its file,
/// whose calls go one at a time.
struct Opened {
    package: Package,
    file: Mutex<Given>,
    /// The package file's memory object, once asked for, and whether it
    /// may be executed.
    memory: Mutex<Option<(OwnedHandle, bool)>>,
}

/// Serves the files of `package`, read through `file`, on `server`.
pub fn serve_package(package: Package, file: Given, server: ServerEnd<Node>) {
    let opened = Arc::new(Opened {
        package,
        file: Mutex::new(file),
        memory: Mutex::new(None),
    });
    spawn(opened, server, String::new(), RX);
}

fn spawn(opened: Arc<Opened>, end: ServerEnd<Node>, path: String, rights: Rights) {
    std::thread::spawn(move || serve(&opened, end, path, rights));
}

/// What a path of the package is.
enum Found<'a> {
    Folder,
    Entry(&'a Entry),
}

impl Opened {
    /// What `path`, relative to the package's root, is.
    fn find(&self, path: &str) -> Option<Found<'_>> {
        if path.is_empty() {
            return Some(Found::Folder);
        }
        match self.package.find(path) {
            Some(entry) if entry.kind == Kind::Folder => Some(Found::Folder),
            Some(entry) => Some(Found::Entry(entry)),
            // A folder that only the paths below it name.
            None => {
                let prefix = format!("{path}/");
                let implied = self
                    .package
                    .entries
                    .iter()
                    .any(|e| e.path.starts_with(&prefix));
                implied.then_some(Found::Folder)
            }
        }
    }

    /// The names in the folder `path`, in order.
    fn names(&self, path: &str) -> Vec<(String, NodeType)> {
        let prefix = match path.is_empty() {
            true => String::new(),
            false => format!("{path}/"),
        };
        let mut names = BTreeSet::new();
        for entry in &self.package.entries {
            let Some(rest) = entry.path.strip_prefix(&prefix) else {
                continue;
            };
            let (name, below) = match rest.split_once('/') {
                Some((name, _)) => (name, true),
                None => (rest, false),
            };
            let kind = match (below, entry.kind) {
                (true, _) | (false, Kind::Folder) => NodeType::Directory,
                (false, Kind::File) => NodeType::File,
                (false, Kind::Symlink) => NodeType::Symlink,
            };
            names.insert((String::from(name), kind_order(kind)));
        }
        names
            .into_iter()
            .map(|(name, kind)| (name, node_type(kind)))
            .collect()
    }

    /// The contents of `entry`.
    fn contents(&self, entry: &Entry) -> Result<Vec<u8>, Error> {
        let file = self.file.lock().map_err(|_| Error::Io)?;
        self.package.read(&*file, entry).map_err(|_| Error::Io)
    }

    /// The bytes of `entry` from `offset`, at most `count`.
    fn read(&self, entry: &Entry, offset: u64, count: u32) -> Result<Vec<u8>, Error> {
        if offset >= entry.size {
            return Ok(Vec::new());
        }
        let count = (count as usize)
            .min(READ_MAX)
            .min((entry.size - offset) as usize);
        if entry.compressed {
            let contents = self.contents(entry)?;
            return Ok(contents[offset as usize..offset as usize + count].to_vec());
        }
        let mut data = vec![0u8; count];
        let file = self.file.lock().map_err(|_| Error::Io)?;
        mikros_mika::format::Source::read_at(&*file, entry.data_offset + offset, &mut data)
            .map_err(|_| Error::Io)?;
        Ok(data)
    }

    /// A memory object of `entry`, executable when `execute`: a clone of
    /// the package file's for a file that is not compressed and lies on
    /// its pages, and otherwise a new object with its contents.
    fn memory(&self, entry: &Entry, read: bool, execute: bool) -> Result<OwnedHandle, Error> {
        let rights = DUPLICATE
            | TRANSFER
            | MAP
            | INSPECT
            | if read { READ } else { 0 }
            | if execute { EXECUTE } else { 0 };
        let len = entry.size.next_multiple_of(PAGE).max(PAGE);
        if !entry.compressed && entry.data_offset.is_multiple_of(PAGE) {
            let package = self.package_memory()?;
            if let Some((mem, executable)) = package.as_ref()
                && (*executable || !execute)
            {
                let mut raw = 0;
                // SAFETY: the out pointer is valid for the call.
                let status =
                    unsafe { sys::mem_clone(mem.raw(), 0, entry.data_offset, len, &mut raw) };
                if status == mikros_rt::OK {
                    // SAFETY: the kernel just made the handle for us.
                    let clone = unsafe { OwnedHandle::from_raw(raw) };
                    return restrict(clone, rights);
                }
            }
        }
        let contents = self.contents(entry)?;
        let mut raw = 0;
        // SAFETY: the out pointer is valid for the call.
        if unsafe { sys::mem_create(len, 0, &mut raw) } != mikros_rt::OK {
            return Err(Error::NoSpace);
        }
        // SAFETY: the kernel just made the handle for us.
        let mem = unsafe { OwnedHandle::from_raw(raw) };
        // SAFETY: the pointer describes `contents`.
        let written =
            unsafe { sys::mem_write(mem.raw(), 0, contents.as_ptr(), contents.len() as u64) };
        if written != mikros_rt::OK {
            return Err(Error::Io);
        }
        // SAFETY: the call takes no pointers.
        unsafe { sys::mem_set_size(mem.raw(), entry.size) };
        restrict(mem, rights)
    }

    /// The package file's memory object, executable when the client's
    /// connection allows it, asked for once.
    fn package_memory(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<(OwnedHandle, bool)>>, Error> {
        let mut memory = self.memory.lock().map_err(|_| Error::Io)?;
        if memory.is_none() {
            let file = self.file.lock().map_err(|_| Error::Io)?;
            let ask = |flags| match file.0.get_memory_object(flags, FOREVER) {
                Ok(Ok(r)) => Some(r.memory),
                _ => None,
            };
            *memory = match ask(MemoryFlags::EXECUTE) {
                Some(mem) => Some((mem, true)),
                None => ask(MemoryFlags(0)).map(|mem| (mem, false)),
            };
        }
        Ok(memory)
    }
}

/// `handle` with only `rights`.
fn restrict(handle: OwnedHandle, rights: u32) -> Result<OwnedHandle, Error> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call; the kernel closes the
    // old handle.
    let status = unsafe { sys::handle_replace(handle.into_raw(), rights, &mut out) };
    if status != mikros_rt::OK {
        return Err(Error::Io);
    }
    // SAFETY: the kernel just made the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}

fn kind_order(kind: NodeType) -> u8 {
    match kind {
        NodeType::Directory => 0,
        NodeType::Symlink => 2,
        _ => 1,
    }
}

fn node_type(order: u8) -> NodeType {
    match order {
        0 => NodeType::Directory,
        2 => NodeType::Symlink,
        _ => NodeType::File,
    }
}

fn attributes(found: &Found<'_>, inode: u64) -> Attributes {
    let (r#type, mode, size) = match found {
        Found::Folder => (NodeType::Directory, 0o555, 0),
        Found::Entry(e) if e.kind == Kind::Symlink => (NodeType::Symlink, 0o777, e.size),
        Found::Entry(e) if e.executable() => (NodeType::File, 0o555, e.size),
        Found::Entry(e) => (NodeType::File, 0o444, e.size),
    };
    Attributes {
        r#type,
        mode,
        owner: 0,
        size,
        allocated: size,
        links: 1,
        inode,
        created: 0,
        modified: 0,
        accessed: 0,
        changed: 0,
    }
}

/// The path that `path` names from the folder `from`, both relative to
/// the package's root, without `.`, or `None` when `..` would leave the
/// root.
fn resolve(from: &str, path: &str) -> Option<String> {
    let mut parts: Vec<&str> = from.split('/').filter(|p| !p.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The error of what the package never does.
fn read_only(rights: Rights) -> Error {
    match rights.contains(Rights::WRITE) {
        true => Error::ReadOnly,
        false => Error::AccessDenied,
    }
}

/// Opens `q.path` from `from`.
fn open(
    opened: &Arc<Opened>,
    from: &str,
    rights: Rights,
    q: NodeOpenRequest,
) -> Result<NodeOpenResponse, Error> {
    let changes = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0 | OpenFlags::APPEND.0);
    if q.flags.0 & changes.0 != 0 || !RX.contains(q.rights) || !rights.contains(q.rights) {
        return Err(read_only(q.rights));
    }
    let path = resolve(from, &q.path).ok_or(Error::NotFound)?;
    let r#type = match opened.find(&path).ok_or(Error::NotFound)? {
        Found::Folder => NodeType::Directory,
        Found::Entry(entry) if entry.kind == Kind::Symlink => {
            if !q.flags.contains(OpenFlags::NO_FOLLOW) {
                let target = opened.contents(entry)?;
                let target = String::from_utf8(target).map_err(|_| Error::Io)?;
                return Ok(NodeOpenResponse {
                    r#type: NodeType::Symlink,
                    redirect: Some(target),
                });
            }
            NodeType::Symlink
        }
        Found::Entry(_) => NodeType::File,
    };
    if r#type != NodeType::Directory && q.flags.contains(OpenFlags::DIRECTORY) {
        return Err(Error::NotDirectory);
    }
    spawn(opened.clone(), q.node, path, q.rights);
    Ok(NodeOpenResponse {
        r#type,
        redirect: None,
    })
}

/// Serves the connection on `end` to `path` of the package with `rights`
/// until the client closes it.
fn serve(opened: &Arc<Opened>, end: ServerEnd<Node>, path: String, rights: Rights) {
    use NodeRequest as R;
    let server = NodeServer::new(end);
    let Some(found) = opened.find(&path) else {
        return;
    };
    let inode = opened
        .package
        .entries
        .iter()
        .position(|e| e.path == path)
        .map_or(1, |at| at as u64 + 2);
    let entry = match &found {
        Found::Entry(entry) => Some(*entry),
        Found::Folder => None,
    };
    let file = entry.filter(|e| e.kind == Kind::File);
    let mut cursor = 0u64;
    while let Ok(request) = server.next(FOREVER) {
        let s = &server;
        // A reply that cannot be sent means that the client went away,
        // which the next read finds.
        let _ = match request {
            R::GetAttributes(_, r) => r.reply(
                s,
                Ok(NodeGetAttributesResponse {
                    attributes: attributes(&found, inode),
                }),
            ),
            R::Clone(q, r) => match rights.contains(q.rights) {
                true => {
                    spawn(opened.clone(), q.node, path.clone(), q.rights);
                    r.reply(s, Ok(NodeCloneResponse {}))
                }
                false => r.reply(s, Err(Error::AccessDenied)),
            },
            // Every process sees the same files.
            R::Restrict(q, r) => {
                spawn(opened.clone(), q.node, path.clone(), rights);
                r.reply(s, Ok(NodeRestrictResponse {}))
            }
            R::Sync(_, r) => r.reply(s, Ok(NodeSyncResponse {})),
            R::QueryFilesystem(_, r) => r.reply(
                s,
                Ok(NodeQueryFilesystemResponse {
                    info: FilesystemInfo {
                        r#type: "package".into(),
                        block_size: PAGE as u32,
                        total_bytes: 0,
                        free_bytes: 0,
                        read_only: true,
                        label: String::new(),
                        id: String::new(),
                    },
                }),
            ),
            R::Open(q, r) => match entry {
                Some(_) => r.reply(s, Err(Error::NotDirectory)),
                None => r.reply(s, open(opened, &path, rights, q)),
            },
            R::ReadDirectory(q, r) => {
                let listed = match (entry, rights.contains(Rights::READ)) {
                    (Some(_), _) => Err(Error::NotDirectory),
                    (None, false) => Err(Error::AccessDenied),
                    (None, true) => {
                        let entries: Vec<DirEntry> = opened
                            .names(&path)
                            .into_iter()
                            .enumerate()
                            .skip(q.cookie as usize)
                            .take(ENTRIES_MAX)
                            .map(|(i, (name, r#type))| DirEntry {
                                name,
                                r#type,
                                inode: i as u64 + 2,
                                attributes: None,
                            })
                            .collect();
                        let next = q.cookie + entries.len() as u64;
                        Ok(NodeReadDirectoryResponse { entries, next })
                    }
                };
                r.reply(s, listed)
            }
            R::Read(q, r) => r.reply(
                s,
                match file {
                    Some(e) => opened
                        .read(e, q.offset, q.count)
                        .map(|data| NodeReadResponse { data }),
                    None => Err(Error::IsDirectory),
                },
            ),
            R::ReadNext(q, r) => r.reply(
                s,
                match file {
                    Some(e) => opened.read(e, cursor, q.count).map(|data| {
                        cursor += data.len() as u64;
                        NodeReadNextResponse { data }
                    }),
                    None => Err(Error::IsDirectory),
                },
            ),
            R::Seek(q, r) => r.reply(
                s,
                match file {
                    Some(e) => {
                        let base = match q.origin {
                            SeekOrigin::Start => Some(0i64),
                            SeekOrigin::Current => Some(cursor as i64),
                            SeekOrigin::End => Some(e.size as i64),
                            SeekOrigin::Unknown(_) => None,
                        };
                        match base
                            .and_then(|base| base.checked_add(q.offset))
                            .filter(|at| *at >= 0)
                        {
                            Some(at) => {
                                cursor = at as u64;
                                Ok(NodeSeekResponse { position: cursor })
                            }
                            None => Err(Error::Invalid),
                        }
                    }
                    None => Err(Error::Invalid),
                },
            ),
            R::ReadLink(_, r) => r.reply(
                s,
                match entry.filter(|e| e.kind == Kind::Symlink) {
                    Some(e) => opened
                        .contents(e)
                        .and_then(|t| String::from_utf8(t).map_err(|_| Error::Io))
                        .map(|target| NodeReadLinkResponse { target }),
                    None => Err(Error::Invalid),
                },
            ),
            R::GetMemoryObject(q, r) => r.reply(
                s,
                match file {
                    Some(_)
                        if !rights.contains(Rights::READ) && !rights.contains(Rights::EXECUTE) =>
                    {
                        Err(Error::AccessDenied)
                    }
                    Some(_) if q.flags.contains(MemoryFlags::WRITE) => Err(read_only(rights)),
                    Some(_)
                        if q.flags.contains(MemoryFlags::EXECUTE)
                            && !rights.contains(Rights::EXECUTE) =>
                    {
                        Err(Error::AccessDenied)
                    }
                    Some(e) => opened
                        .memory(
                            e,
                            rights.contains(Rights::READ),
                            q.flags.contains(MemoryFlags::EXECUTE),
                        )
                        .map(|memory| NodeGetMemoryObjectResponse { memory }),
                    None => Err(Error::IsDirectory),
                },
            ),
            R::GetRevocableMemoryObject(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::GetAcl(_, r) => r.reply(s, Err(Error::NotSupported)),
            // Without lists, the connection's rights alone decide.
            R::Access(_, r) => r.reply(s, Ok(NodeAccessResponse { rights })),
            R::Lock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unlock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Watch(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unwatch(_, r) => r.reply(s, Err(Error::NotSupported)),
            // Everything that would change the package.
            R::Write(_, r) => r.reply(s, Err(read_only(rights))),
            R::WriteNext(_, r) => r.reply(s, Err(read_only(rights))),
            R::Append(_, r) => r.reply(s, Err(read_only(rights))),
            R::Truncate(_, r) => r.reply(s, Err(read_only(rights))),
            R::SetOwner(_, r) => r.reply(s, Err(read_only(rights))),
            R::SetAcl(_, r) => r.reply(s, Err(read_only(rights))),
            R::SetAttributes(_, r) => r.reply(s, Err(read_only(rights))),
            R::CloneRange(_, r) => r.reply(s, Err(read_only(rights))),
            R::Unlink(_, r) => r.reply(s, Err(read_only(rights))),
            R::GetToken(_, r) => r.reply(s, Err(read_only(rights))),
            R::Rename(_, r) => r.reply(s, Err(read_only(rights))),
            R::Link(_, r) => r.reply(s, Err(read_only(rights))),
            R::CreateSymlink(_, r) => r.reply(s, Err(read_only(rights))),
            R::BindSocket(_, r) => r.reply(s, Err(read_only(rights))),
            R::ConnectSocket(_, r) => r.reply(s, Err(read_only(rights))),
            R::Exchange(_, r) => r.reply(s, Err(read_only(rights))),
        };
    }
}
