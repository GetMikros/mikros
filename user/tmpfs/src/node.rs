//! One connection to a node of the file system.

use crate::file::{Failed, File};
use crate::fs::{self, Fs};
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_memfs::{Directory, Id, Kind, Resolved};
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::*;

/// The most bytes one read returns, the limit of its reply.
const READ_MAX: u32 = 32768;
/// The most entries one directory read returns, the limit of its reply.
const ENTRIES_MAX: usize = 64;
/// The bits of a mode that are stored.
const MODE_BITS: u32 = 0o7777;

struct Connection {
    fs: Rc<Fs>,
    node: Id,
    rights: Rights,
    /// The user that nodes made through the connection belong to.
    user: u32,
    cursor: u64,
    append: bool,
}

/// Serves the connection on `end` to `node` until the client closes it or
/// sends an invalid message, and then lets go of `node`.
pub async fn serve(
    fs: Rc<Fs>,
    end: ServerEnd<Node>,
    node: Id,
    rights: Rights,
    user: u32,
    append: bool,
) {
    let server = NodeAsyncServer::new(end, fs.reactor.clone());
    let mut connection = Connection {
        fs,
        node,
        rights,
        user,
        cursor: 0,
        append,
    };
    while let Ok(request) = server.next().await {
        connection.handle(request, server.server());
    }
    connection.fs.close(node);
}

fn failed(e: Failed) -> Error {
    match e {
        Failed::NoSpace => Error::NoSpace,
        Failed::Io => Error::Io,
    }
}

impl Connection {
    fn handle(&mut self, request: NodeRequest, s: &NodeServer) {
        use NodeRequest as R;
        // A reply that cannot be sent means that the client went away,
        // which the next read finds.
        let _ = match request {
            R::GetAttributes(_, r) => {
                let attributes = self.attributes(self.node);
                r.reply(
                    s,
                    attributes.map(|attributes| NodeGetAttributesResponse { attributes }),
                )
            }
            R::SetAttributes(q, r) => r.reply(s, self.set_attributes(q)),
            R::Clone(q, r) => r.reply(s, self.clone(q)),
            // Nothing is ever more durable than memory.
            R::Sync(_, r) => r.reply(s, Ok(NodeSyncResponse {})),
            R::QueryFilesystem(_, r) => r.reply(
                s,
                Ok(NodeQueryFilesystemResponse {
                    info: self.fs.info(),
                }),
            ),
            R::Read(q, r) => r.reply(
                s,
                self.read(q.offset, q.count)
                    .map(|data| NodeReadResponse { data }),
            ),
            R::ReadNext(q, r) => r.reply(s, self.read_next(q.count)),
            R::Write(q, r) => {
                let written = self
                    .write(Some(q.offset), &q.data)
                    .map(|_| q.data.len() as u32);
                r.reply(s, written.map(|written| NodeWriteResponse { written }))
            }
            R::Append(q, r) => r.reply(
                s,
                self.write(None, &q.data)
                    .map(|offset| NodeAppendResponse { offset }),
            ),
            R::WriteNext(q, r) => r.reply(s, self.write_next(&q.data)),
            R::Truncate(q, r) => {
                r.reply(s, self.truncate(q.size).map(|()| NodeTruncateResponse {}))
            }
            R::Seek(q, r) => r.reply(s, self.seek(q.offset, q.origin)),
            R::Open(q, r) => r.reply(s, self.open(q)),
            R::ReadDirectory(q, r) => r.reply(s, self.read_directory(q.cookie, q.attributes)),
            R::Unlink(q, r) => r.reply(
                s,
                self.unlink(&q.name, q.directory)
                    .map(|()| NodeUnlinkResponse {}),
            ),
            R::GetToken(_, r) => r.reply(s, self.token()),
            R::Rename(q, r) => r.reply(s, self.rename(q).map(|()| NodeRenameResponse {})),
            R::Link(q, r) => r.reply(s, self.link(q).map(|()| NodeLinkResponse {})),
            R::CreateSymlink(q, r) => {
                let created = self.create_symlink(&q.name, q.target);
                r.reply(s, created.map(|()| NodeCreateSymlinkResponse {}))
            }
            R::ReadLink(_, r) => r.reply(s, self.read_link()),
            // Memory cannot share data between files.
            R::CloneRange(_, r) => r.reply(s, Err(Error::NotSupported)),
            // Not there yet.
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

    fn attributes(&self, id: Id) -> Result<Attributes, Error> {
        let tree = self.fs.tree.borrow();
        let node = tree.get(id).ok_or(Error::NotFound)?;
        let (r#type, size, allocated) = match &node.kind {
            Kind::Directory(_) => (NodeType::Directory, 0, 0),
            Kind::File(f) => (NodeType::File, f.size, f.allocated()),
            Kind::Symlink(t) => (NodeType::Symlink, t.len() as u64, 0),
            Kind::Socket(_) => (NodeType::Socket, 0, 0),
        };
        Ok(Attributes {
            r#type,
            mode: node.mode,
            owner: node.owner,
            size,
            allocated,
            links: tree.links(id),
            inode: id,
            created: node.times.created,
            modified: node.times.modified,
            accessed: node.times.accessed,
            changed: node.times.changed,
        })
    }

    fn set_attributes(
        &self,
        q: NodeSetAttributesRequest,
    ) -> Result<NodeSetAttributesResponse, Error> {
        self.require(Rights::WRITE)?;
        let mut tree = self.fs.tree.borrow_mut();
        let node = tree.get_mut(self.node).ok_or(Error::NotFound)?;
        if q.mask.contains(AttributeMask::MODE) {
            node.mode = q.mode & MODE_BITS;
        }
        if q.mask.contains(AttributeMask::ACCESSED) {
            node.times.accessed = q.accessed;
        }
        if q.mask.contains(AttributeMask::MODIFIED) {
            node.times.modified = q.modified;
        }
        node.times.changed = fs::now();
        Ok(NodeSetAttributesResponse {})
    }

    fn clone(&self, q: NodeCloneRequest) -> Result<NodeCloneResponse, Error> {
        self.require(q.rights)?;
        self.fs
            .serve(q.node, self.node, q.rights, self.user, self.append);
        Ok(NodeCloneResponse {})
    }

    /// Runs `f` on the file of this connection.
    fn with_file<R>(&self, f: impl FnOnce(&mut File) -> Result<R, Error>) -> Result<R, Error> {
        let mut tree = self.fs.tree.borrow_mut();
        match &mut tree.get_mut(self.node).ok_or(Error::NotFound)?.kind {
            Kind::File(file) => f(file),
            Kind::Directory(_) => Err(Error::IsDirectory),
            _ => Err(Error::Invalid),
        }
    }

    /// Marks this connection's node modified now.
    fn modified(&self) {
        if let Some(node) = self.fs.tree.borrow_mut().get_mut(self.node) {
            let now = fs::now();
            node.times.modified = now;
            node.times.changed = now;
        }
    }

    fn read(&self, offset: u64, count: u32) -> Result<Vec<u8>, Error> {
        self.require(Rights::READ)?;
        let count = count.min(READ_MAX) as usize;
        self.with_file(|file| file.read(offset, count).map_err(failed))
    }

    fn read_next(&mut self, count: u32) -> Result<NodeReadNextResponse, Error> {
        let data = self.read(self.cursor, count)?;
        self.cursor += data.len() as u64;
        Ok(NodeReadNextResponse { data })
    }

    /// Writes `data` at `offset`, or at the end without one, and returns
    /// where it went.
    fn write(&self, offset: Option<u64>, data: &[u8]) -> Result<u64, Error> {
        self.require(Rights::WRITE)?;
        let at = self.with_file(|file| {
            let at = offset.unwrap_or(file.size);
            if at
                .checked_add(data.len() as u64)
                .is_none_or(|end| end > i64::MAX as u64)
            {
                return Err(Error::TooBig);
            }
            file.write(at, data).map(|()| at).map_err(failed)
        })?;
        self.modified();
        Ok(at)
    }

    fn write_next(&mut self, data: &[u8]) -> Result<NodeWriteNextResponse, Error> {
        let offset = if self.append { None } else { Some(self.cursor) };
        let at = self.write(offset, data)?;
        self.cursor = at + data.len() as u64;
        Ok(NodeWriteNextResponse {
            written: data.len() as u32,
        })
    }

    fn truncate(&self, size: u64) -> Result<(), Error> {
        self.require(Rights::WRITE)?;
        if size > i64::MAX as u64 {
            return Err(Error::TooBig);
        }
        self.with_file(|file| file.truncate(size).map_err(failed))?;
        self.modified();
        Ok(())
    }

    fn seek(&mut self, offset: i64, origin: SeekOrigin) -> Result<NodeSeekResponse, Error> {
        let size = self
            .with_file(|file| Ok(file.size))
            .map_err(|_| Error::Invalid)?;
        let base = match origin {
            SeekOrigin::Start => 0,
            SeekOrigin::Current => self.cursor,
            SeekOrigin::End => size,
            SeekOrigin::Unknown(_) => return Err(Error::Invalid),
        };
        let position = i128::from(base) + i128::from(offset);
        self.cursor = u64::try_from(position).map_err(|_| Error::Invalid)?;
        Ok(NodeSeekResponse {
            position: self.cursor,
        })
    }

    fn is_directory(&self, id: Id) -> bool {
        self.fs
            .tree
            .borrow()
            .get(id)
            .is_some_and(|n| n.is_directory())
    }

    fn open(&self, q: NodeOpenRequest) -> Result<NodeOpenResponse, Error> {
        self.require(q.rights)?;
        if !self.is_directory(self.node) {
            return Err(Error::NotDirectory);
        }
        let no_follow = q.flags.contains(OpenFlags::NO_FOLLOW);
        let want_dir = q.flags.contains(OpenFlags::DIRECTORY) || q.path.ends_with('/');
        let resolved = self
            .fs
            .tree
            .borrow()
            .resolve(self.node, &q.path, no_follow)
            .map_err(fs::error)?;
        let id = match resolved {
            Resolved::Redirect(target) => {
                return Ok(NodeOpenResponse {
                    r#type: NodeType::Symlink,
                    redirect: Some(target),
                });
            }
            Resolved::Missing { parent, name } if q.flags.contains(OpenFlags::CREATE) => {
                self.require(Rights::WRITE)?;
                let kind = match q.flags.contains(OpenFlags::DIRECTORY) {
                    true => Kind::Directory(Directory::default()),
                    false => Kind::File(File::default()),
                };
                let mode = q.mode & MODE_BITS;
                let mut tree = self.fs.tree.borrow_mut();
                tree.create(parent, &name, kind, mode, self.user, fs::now())
                    .map_err(fs::error)?
            }
            Resolved::Missing { .. } => return Err(Error::NotFound),
            Resolved::Node(_) if q.flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUSIVE) => {
                return Err(Error::Exists);
            }
            Resolved::Node(id) => id,
        };
        let r#type = self.attributes(id)?.r#type;
        if want_dir && r#type != NodeType::Directory {
            return Err(Error::NotDirectory);
        }
        if r#type == NodeType::Socket {
            return Err(Error::Invalid);
        }
        if q.flags.contains(OpenFlags::TRUNCATE) && r#type == NodeType::File {
            self.require(Rights::WRITE)?;
            let mut tree = self.fs.tree.borrow_mut();
            if let Some(Kind::File(file)) = tree.get_mut(id).map(|n| &mut n.kind) {
                file.truncate(0).map_err(failed)?;
            }
        }
        let append = q.flags.contains(OpenFlags::APPEND);
        self.fs.serve(q.node, id, q.rights, self.user, append);
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
        let (found, next) = self
            .fs
            .tree
            .borrow()
            .list(self.node, cookie, ENTRIES_MAX)
            .map_err(fs::error)?;
        let mut entries = Vec::with_capacity(found.len());
        for (name, id) in found {
            let attrs = self.attributes(id)?;
            entries.push(DirEntry {
                name,
                r#type: attrs.r#type,
                inode: id,
                attributes: attributes.then(|| Box::new(attrs)),
            });
        }
        Ok(NodeReadDirectoryResponse { entries, next })
    }

    fn unlink(&self, name: &str, directory: bool) -> Result<(), Error> {
        self.require(Rights::WRITE)?;
        let removed = self
            .fs
            .tree
            .borrow_mut()
            .unlink(self.node, name, directory, fs::now());
        let removed = removed.map_err(fs::error)?;
        if removed.last {
            self.fs.unnamed(removed.id);
        }
        Ok(())
    }

    fn token(&self) -> Result<NodeGetTokenResponse, Error> {
        self.require(Rights::WRITE)?;
        if !self.is_directory(self.node) {
            return Err(Error::NotDirectory);
        }
        Ok(NodeGetTokenResponse {
            token: self.fs.token(self.node)?,
        })
    }

    fn rename(&self, q: NodeRenameRequest) -> Result<(), Error> {
        self.require(Rights::WRITE)?;
        let to = self.fs.token_dir(&q.target).ok_or(Error::CrossDevice)?;
        let renamed =
            self.fs
                .tree
                .borrow_mut()
                .rename(self.node, &q.name, to, &q.new_name, fs::now());
        if let Some(replaced) = renamed.map_err(fs::error)?
            && replaced.last
        {
            self.fs.unnamed(replaced.id);
        }
        Ok(())
    }

    fn link(&self, q: NodeLinkRequest) -> Result<(), Error> {
        self.require(Rights::WRITE)?;
        let to = self.fs.token_dir(&q.target).ok_or(Error::CrossDevice)?;
        let linked = self
            .fs
            .tree
            .borrow_mut()
            .link(self.node, &q.name, to, &q.new_name, fs::now());
        linked.map(|_| ()).map_err(fs::error)
    }

    fn create_symlink(&self, name: &str, target: String) -> Result<(), Error> {
        self.require(Rights::WRITE)?;
        if target.is_empty() {
            return Err(Error::Invalid);
        }
        let mut tree = self.fs.tree.borrow_mut();
        let created = tree.create(
            self.node,
            name,
            Kind::Symlink(target),
            0o777,
            self.user,
            fs::now(),
        );
        created.map(|_| ()).map_err(fs::error)
    }

    fn read_link(&self) -> Result<NodeReadLinkResponse, Error> {
        self.require(Rights::READ)?;
        match &self
            .fs
            .tree
            .borrow()
            .get(self.node)
            .ok_or(Error::NotFound)?
            .kind
        {
            Kind::Symlink(target) => Ok(NodeReadLinkResponse {
                target: target.clone(),
            }),
            _ => Err(Error::Invalid),
        }
    }
}
