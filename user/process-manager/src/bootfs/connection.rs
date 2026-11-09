//! One connection to a node of the boot file system.

use super::Fs;
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_bootfs::{NodeKind, Resolved};
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::*;

/// The most bytes one read returns, the limit of its reply.
const READ_MAX: u32 = 32768;
/// The most entries one directory read returns, the limit of its reply.
const ENTRIES_MAX: usize = 64;
/// The block size reported, that of the archive.
const BLOCK: u64 = 512;
const NANOSECONDS: i64 = 1_000_000_000;

struct Connection {
    fs: Rc<Fs>,
    node: usize,
    rights: Rights,
    cursor: u64,
}

/// Serves the connection on `end` to `node` with `rights` until the
/// client closes it or sends an invalid message.
pub(super) async fn serve(fs: Rc<Fs>, end: ServerEnd<Node>, node: usize, rights: Rights) {
    let server = NodeAsyncServer::new(end, fs.reactor.clone());
    let mut connection = Connection {
        fs,
        node,
        rights,
        cursor: 0,
    };
    while let Ok(request) = server.next().await {
        connection.handle(request, server.server());
    }
}

impl Connection {
    fn handle(&mut self, request: NodeRequest, s: &NodeServer) {
        use NodeRequest as R;
        // A reply that cannot be sent means that the client went away,
        // which the next read finds.
        let _ = match request {
            R::GetAttributes(_, r) => r.reply(
                s,
                Ok(NodeGetAttributesResponse {
                    attributes: self.attributes(self.node),
                }),
            ),
            R::Clone(q, r) => r.reply(s, self.clone(q)),
            R::Sync(_, r) => r.reply(s, Ok(NodeSyncResponse {})),
            R::QueryFilesystem(_, r) => r.reply(
                s,
                Ok(NodeQueryFilesystemResponse {
                    info: self.filesystem(),
                }),
            ),
            R::Read(q, r) => r.reply(
                s,
                self.read(q.offset, q.count)
                    .map(|data| NodeReadResponse { data }),
            ),
            R::ReadNext(q, r) => r.reply(s, self.read_next(q.count)),
            R::Seek(q, r) => r.reply(s, self.seek(q.offset, q.origin)),
            R::Open(q, r) => r.reply(s, self.open(q)),
            R::ReadDirectory(q, r) => r.reply(s, self.read_directory(q.cookie, q.attributes)),
            R::ReadLink(_, r) => r.reply(s, self.read_link()),
            R::GetMemoryObject(q, r) => r.reply(s, self.memory(q.flags)),
            R::Lock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unlock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Watch(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unwatch(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::GetRevocableMemoryObject(_, r) => r.reply(s, Err(Error::NotSupported)),
            // Everything that would change the file system.
            R::SetAttributes(_, r) => r.reply(s, Err(self.write_denied())),
            R::Write(_, r) => r.reply(s, Err(self.write_denied())),
            R::Append(_, r) => r.reply(s, Err(self.write_denied())),
            R::Truncate(_, r) => r.reply(s, Err(self.write_denied())),
            R::WriteNext(_, r) => r.reply(s, Err(self.write_denied())),
            R::CloneRange(_, r) => r.reply(s, Err(self.write_denied())),
            R::Unlink(_, r) => r.reply(s, Err(self.write_denied())),
            R::GetToken(_, r) => r.reply(s, Err(self.write_denied())),
            R::Rename(_, r) => r.reply(s, Err(self.write_denied())),
            R::Link(_, r) => r.reply(s, Err(self.write_denied())),
            R::CreateSymlink(_, r) => r.reply(s, Err(self.write_denied())),
            R::BindSocket(_, r) => r.reply(s, Err(self.write_denied())),
            R::ConnectSocket(_, r) => r.reply(s, Err(self.write_denied())),
        };
    }

    /// The error of an operation that would change the file system:
    /// without `WRITE` the right is missing, and with it the file system
    /// is still read-only.
    fn write_denied(&self) -> Error {
        match self.rights.contains(Rights::WRITE) {
            true => Error::ReadOnly,
            false => Error::AccessDenied,
        }
    }

    fn require(&self, rights: Rights) -> Result<(), Error> {
        match self.rights.contains(rights) {
            true => Ok(()),
            false => Err(Error::AccessDenied),
        }
    }

    fn kind(&self, node: usize) -> &NodeKind {
        &self.fs.tree.nodes[node].kind
    }

    fn attributes(&self, node: usize) -> Attributes {
        let n = &self.fs.tree.nodes[node];
        let (kind, size) = match &n.kind {
            NodeKind::Directory(_) => (NodeType::Directory, 0),
            NodeKind::File { size, .. } => (NodeType::File, *size as u64),
            NodeKind::Symlink(target) => (NodeType::Symlink, target.len() as u64),
        };
        let time = i64::try_from(n.mtime)
            .unwrap_or(i64::MAX)
            .saturating_mul(NANOSECONDS);
        Attributes {
            r#type: kind,
            mode: n.mode,
            owner: n.owner,
            size,
            allocated: size.next_multiple_of(BLOCK),
            links: n.links,
            inode: node as u64 + 1,
            created: time,
            modified: time,
            accessed: time,
            changed: time,
        }
    }

    fn filesystem(&self) -> FilesystemInfo {
        FilesystemInfo {
            r#type: "bootfs".into(),
            block_size: BLOCK as u32,
            total_bytes: self.fs.image.len() as u64,
            free_bytes: 0,
            read_only: true,
            label: String::new(),
        }
    }

    fn clone(&self, q: NodeCloneRequest) -> Result<NodeCloneResponse, Error> {
        self.require(q.rights)?;
        self.fs.serve(q.node, self.node, q.rights);
        Ok(NodeCloneResponse {})
    }

    /// The bytes of the file from `offset`, at most `count` of them.
    fn read(&self, offset: u64, count: u32) -> Result<Vec<u8>, Error> {
        self.require(Rights::READ)?;
        let (start, size) = match self.kind(self.node) {
            NodeKind::File { offset, size } => (*offset, *size),
            NodeKind::Directory(_) => return Err(Error::IsDirectory),
            NodeKind::Symlink(_) => return Err(Error::Invalid),
        };
        let from = usize::try_from(offset).unwrap_or(usize::MAX).min(size);
        let to = from.saturating_add(count.min(READ_MAX) as usize).min(size);
        Ok(self.fs.image[start + from..start + to].to_vec())
    }

    fn read_next(&mut self, count: u32) -> Result<NodeReadNextResponse, Error> {
        let data = self.read(self.cursor, count)?;
        self.cursor += data.len() as u64;
        Ok(NodeReadNextResponse { data })
    }

    fn seek(&mut self, offset: i64, origin: SeekOrigin) -> Result<NodeSeekResponse, Error> {
        let NodeKind::File { size, .. } = self.kind(self.node) else {
            return Err(Error::Invalid);
        };
        let base = match origin {
            SeekOrigin::Start => 0,
            SeekOrigin::Current => self.cursor,
            SeekOrigin::End => *size as u64,
            SeekOrigin::Unknown(_) => return Err(Error::Invalid),
        };
        let position = i128::from(base) + i128::from(offset);
        self.cursor = u64::try_from(position).map_err(|_| Error::Invalid)?;
        Ok(NodeSeekResponse {
            position: self.cursor,
        })
    }

    fn open(&self, q: NodeOpenRequest) -> Result<NodeOpenResponse, Error> {
        self.require(q.rights)?;
        if !matches!(self.kind(self.node), NodeKind::Directory(_)) {
            return Err(Error::NotDirectory);
        }
        let no_follow = q.flags.contains(OpenFlags::NO_FOLLOW);
        let resolved = self
            .fs
            .tree
            .resolve(self.node, q.path.as_bytes(), no_follow);
        let node = match resolved {
            Ok(Resolved::Node(node)) => node,
            Ok(Resolved::Redirect(target)) => {
                return Ok(NodeOpenResponse {
                    r#type: NodeType::Symlink,
                    redirect: Some(String::from_utf8_lossy(&target).into_owned()),
                });
            }
            Err(mikros_bootfs::Error::NotFound) if q.flags.contains(OpenFlags::CREATE) => {
                return Err(self.write_denied());
            }
            Err(e) => return Err(resolve_error(e)),
        };
        if q.flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUSIVE) {
            return Err(Error::Exists);
        }
        let directory = matches!(self.kind(node), NodeKind::Directory(_));
        if q.flags.contains(OpenFlags::DIRECTORY) && !directory {
            return Err(Error::NotDirectory);
        }
        if q.flags.contains(OpenFlags::TRUNCATE) {
            return Err(self.write_denied());
        }
        let r#type = self.attributes(node).r#type;
        self.fs.serve(q.node, node, q.rights);
        Ok(NodeOpenResponse {
            r#type,
            redirect: None,
        })
    }

    fn read_directory(
        &self,
        cookie: u64,
        attributes: bool,
    ) -> Result<NodeReadDirectoryResponse, Error> {
        self.require(Rights::READ)?;
        if !matches!(self.kind(self.node), NodeKind::Directory(_)) {
            return Err(Error::NotDirectory);
        }
        let (found, next) = self.fs.tree.read_directory(self.node, cookie, ENTRIES_MAX);
        let entries = found
            .into_iter()
            .map(|(name, node)| {
                let attrs = self.attributes(node);
                DirEntry {
                    name: String::from_utf8_lossy(name).into_owned(),
                    r#type: attrs.r#type,
                    inode: attrs.inode,
                    attributes: attributes.then(|| Box::new(attrs)),
                }
            })
            .collect();
        Ok(NodeReadDirectoryResponse { entries, next })
    }

    /// A memory object with the file's contents: with `PRIVATE` a copy
    /// of its own, and otherwise the file's paged object, which is never
    /// writable.
    fn memory(&self, flags: MemoryFlags) -> Result<NodeGetMemoryObjectResponse, Error> {
        self.require(Rights::READ)?;
        let (offset, size) = match self.kind(self.node) {
            NodeKind::File { offset, size } => (*offset, *size),
            NodeKind::Directory(_) => return Err(Error::IsDirectory),
            NodeKind::Symlink(_) => return Err(Error::Invalid),
        };
        let execute = flags.contains(MemoryFlags::EXECUTE);
        if execute {
            self.require(Rights::EXECUTE)?;
        }
        let write = flags.contains(MemoryFlags::WRITE);
        let memory = if flags.contains(MemoryFlags::PRIVATE) {
            let rights = super::memory::rights(write, execute);
            self.fs.private_memory(offset, size, rights)
        } else if write {
            return Err(self.write_denied());
        } else {
            let rights = super::memory::rights(false, execute);
            self.fs.paged_memory(self.node, offset, size, rights)
        };
        match memory {
            Ok(memory) => Ok(NodeGetMemoryObjectResponse { memory }),
            Err(status) => {
                mikros_rt::log!(Error, "a boot file's memory object failed: {status}");
                Err(Error::Io)
            }
        }
    }

    fn read_link(&self) -> Result<NodeReadLinkResponse, Error> {
        self.require(Rights::READ)?;
        match self.kind(self.node) {
            NodeKind::Symlink(target) => Ok(NodeReadLinkResponse {
                target: String::from_utf8_lossy(target).into_owned(),
            }),
            _ => Err(Error::Invalid),
        }
    }
}

fn resolve_error(e: mikros_bootfs::Error) -> Error {
    use mikros_bootfs::Error as E;
    match e {
        E::NotFound => Error::NotFound,
        E::NotDirectory => Error::NotDirectory,
        E::Invalid => Error::Invalid,
        E::AccessDenied => Error::AccessDenied,
        E::NameTooLong => Error::NameTooLong,
        E::Loop => Error::Loop,
    }
}
