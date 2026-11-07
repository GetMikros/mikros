//! One connection to a node of the volume.

use crate::fs::{self, Fs, Target};
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::*;

/// The most bytes one read returns, the limit of its reply.
const READ_MAX: u32 = 32768;
/// The most entries one directory read returns, the limit of its reply.
const ENTRIES_MAX: usize = 64;
/// The longest path component, in bytes.
const NAME_MAX: usize = 255;

struct Connection {
    fs: Rc<Fs>,
    target: Target,
    rights: Rights,
    cursor: u64,
    append: bool,
}

/// Serves the connection on `end` to `target` with `rights` until the
/// client closes it or sends an invalid message, and then lets go of
/// `target`.
pub async fn serve(fs: Rc<Fs>, end: ServerEnd<Node>, target: Target, rights: Rights, append: bool) {
    let server = NodeAsyncServer::new(end, fs.reactor.clone());
    let mut connection = Connection {
        fs,
        target,
        rights,
        cursor: 0,
        append,
    };
    while let Ok(request) = server.next().await {
        connection.handle(request, server.server());
    }
    connection.fs.close(connection.target);
}

impl Connection {
    fn handle(&mut self, request: NodeRequest, s: &NodeServer) {
        use NodeRequest as R;
        // A reply that cannot be sent means that the client went away,
        // which the next read finds.
        let _ = match request {
            R::GetAttributes(_, r) => {
                let attributes = self.fs.attributes(self.target);
                r.reply(
                    s,
                    attributes.map(|attributes| NodeGetAttributesResponse { attributes }),
                )
            }
            R::Clone(q, r) => r.reply(s, self.clone(q)),
            R::Sync(_, r) => {
                let synced = self.fs.volume.borrow_mut().sync().map_err(fs::error);
                r.reply(s, synced.map(|()| NodeSyncResponse {}))
            }
            R::QueryFilesystem(_, r) => r.reply(
                s,
                self.fs
                    .info()
                    .map(|info| NodeQueryFilesystemResponse { info }),
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
            R::ReadLink(_, r) => r.reply(s, Err(Error::Invalid)),
            // Everything that would change the volume, which is served
            // read-only.
            R::SetAttributes(_, r) => r.reply(s, Err(self.write_denied())),
            R::Write(_, r) => r.reply(s, Err(self.write_denied())),
            R::Append(_, r) => r.reply(s, Err(self.write_denied())),
            R::WriteNext(_, r) => r.reply(s, Err(self.write_denied())),
            R::Truncate(_, r) => r.reply(s, Err(self.write_denied())),
            R::Unlink(_, r) => r.reply(s, Err(self.write_denied())),
            R::GetToken(_, r) => r.reply(s, Err(self.write_denied())),
            R::Rename(_, r) => r.reply(s, Err(self.write_denied())),
            // What FAT cannot hold.
            R::CloneRange(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Link(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::CreateSymlink(_, r) => r.reply(s, Err(Error::NotSupported)),
            // What this server does not provide.
            R::GetMemoryObject(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::GetRevocableMemoryObject(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Lock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unlock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Watch(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unwatch(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::BindSocket(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::ConnectSocket(_, r) => r.reply(s, Err(Error::NotSupported)),
        };
    }

    fn require(&self, rights: Rights) -> Result<(), Error> {
        match self.rights.contains(rights) {
            true => Ok(()),
            false => Err(Error::AccessDenied),
        }
    }

    /// The error of an operation that would change the volume: without
    /// `WRITE` the right is missing, and with it the volume is read-only.
    fn write_denied(&self) -> Error {
        match self.rights.contains(Rights::WRITE) {
            true => Error::ReadOnly,
            false => Error::AccessDenied,
        }
    }

    /// The node of a file connection.
    fn file(&self) -> Result<u64, Error> {
        match self.target {
            Target::Root => Err(Error::IsDirectory),
            Target::Node(id) => match self.fs.is_directory(self.target)? {
                true => Err(Error::IsDirectory),
                false => Ok(id),
            },
        }
    }

    fn clone(&self, q: NodeCloneRequest) -> Result<NodeCloneResponse, Error> {
        self.require(q.rights)?;
        self.fs.serve(q.node, self.target, q.rights, self.append);
        Ok(NodeCloneResponse {})
    }

    /// The bytes of the file from `offset`, at most `count` of them.
    fn read(&self, offset: u64, count: u32) -> Result<Vec<u8>, Error> {
        self.require(Rights::READ)?;
        let id = self.file()?;
        let mut data = vec![0u8; count.min(READ_MAX) as usize];
        let n = self
            .fs
            .with_node(id, |volume, node| volume.read(node, offset, &mut data))?;
        data.truncate(n);
        Ok(data)
    }

    fn read_next(&mut self, count: u32) -> Result<NodeReadNextResponse, Error> {
        let data = self.read(self.cursor, count)?;
        self.cursor += data.len() as u64;
        Ok(NodeReadNextResponse { data })
    }

    fn seek(&mut self, offset: i64, origin: SeekOrigin) -> Result<NodeSeekResponse, Error> {
        let id = self.file().map_err(|_| Error::Invalid)?;
        let base = match origin {
            SeekOrigin::Start => 0,
            SeekOrigin::Current => self.cursor,
            SeekOrigin::End => self.fs.with_node(id, |_, node| Ok(node.size()))?,
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
        if q.rights.contains(Rights::WRITE) && self.fs.read_only() {
            return Err(Error::ReadOnly);
        }
        let mut dir = self.fs.dir(self.target)?;
        let (parts, trailing) = components(&q.path)?;
        let root = self.fs.volume.borrow().root();
        let Some((&last, within)) = parts.split_last() else {
            // The path names this directory itself.
            self.fs.serve(q.node, self.target, q.rights, false);
            return Ok(NodeOpenResponse {
                r#type: NodeType::Directory,
                redirect: None,
            });
        };
        for part in within {
            let mut volume = self.fs.volume.borrow_mut();
            let node = volume.find(dir, part).map_err(fs::error)?;
            dir = node.as_dir(root).ok_or(Error::NotDirectory)?;
        }
        let want_dir = q.flags.contains(OpenFlags::DIRECTORY) || trailing;
        let found = self.fs.volume.borrow_mut().find(dir, last);
        let node = match found {
            Ok(_) if q.flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUSIVE) => {
                return Err(Error::Exists);
            }
            Ok(node) if want_dir && !node.is_directory() => return Err(Error::NotDirectory),
            Ok(node) => node,
            Err(mikros_fat::Error::NotFound) if q.flags.contains(OpenFlags::CREATE) => {
                return Err(self.write_denied());
            }
            Err(e) => return Err(fs::error(e)),
        };
        let r#type = if node.is_directory() {
            NodeType::Directory
        } else {
            NodeType::File
        };
        let target = self.fs.target(node);
        if q.flags.contains(OpenFlags::TRUNCATE) && r#type == NodeType::File {
            return Err(self.write_denied());
        }
        let append = q.flags.contains(OpenFlags::APPEND);
        self.fs.serve(q.node, target, q.rights, append);
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
        let dir = self.fs.dir(self.target)?;
        let Ok(from) = u32::try_from(cookie) else {
            return Ok(NodeReadDirectoryResponse {
                entries: Vec::new(),
                next: cookie,
            });
        };
        let mut volume = self.fs.volume.borrow_mut();
        let (nodes, next) = volume.list(dir, from, ENTRIES_MAX).map_err(fs::error)?;
        let mut entries = Vec::with_capacity(nodes.len());
        for node in nodes {
            let attrs = fs::node_attributes(&mut volume, &node).map_err(fs::error)?;
            entries.push(DirEntry {
                name: node.name,
                r#type: attrs.r#type,
                inode: attrs.inode,
                attributes: attributes.then(|| Box::new(attrs)),
            });
        }
        Ok(NodeReadDirectoryResponse {
            entries,
            next: u64::from(next),
        })
    }
}

/// The components of the path of `Open`, without `.`, and whether it
/// ends in `/`.
fn components(path: &str) -> Result<(Vec<&str>, bool), Error> {
    if path.is_empty() || path.starts_with('/') {
        return Err(Error::Invalid);
    }
    let trailing = path.ends_with('/');
    let path = path.strip_suffix('/').unwrap_or(path);
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" => return Err(Error::Invalid),
            ".." => return Err(Error::AccessDenied),
            "." => continue,
            p if p.len() > NAME_MAX => return Err(Error::NameTooLong),
            p => parts.push(p),
        }
    }
    Ok((parts, trailing))
}
