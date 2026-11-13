//! One connection to a node of the volume.

use crate::format::Format;
use crate::fs::{self, Fs, Target};
use crate::memory;
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::future::poll_fn;
use core::task::Poll;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::*;

/// The most bytes one read returns, the limit of its reply.
const READ_MAX: u32 = 32768;
/// The most entries one directory read returns, the limit of its reply.
const ENTRIES_MAX: usize = 64;
/// The longest path component, in bytes.
const NAME_MAX: usize = 255;

struct Connection<F: Format> {
    fs: Rc<Fs<F>>,
    target: Target,
    rights: Rights,
    cursor: u64,
    append: bool,
    /// The owner of the connection's locks.
    owner: u64,
}

/// Serves the connection on `end` to `target` with `rights` until the
/// client closes it or sends an invalid message, and then lets go of
/// `target`.
pub async fn serve<F: Format>(
    fs: Rc<Fs<F>>,
    end: ServerEnd<Node>,
    target: Target,
    rights: Rights,
    append: bool,
) {
    let server = NodeAsyncServer::new(end, fs.reactor.clone());
    let owner = fs.owner();
    let mut connection = Connection {
        fs,
        target,
        rights,
        cursor: 0,
        append,
        owner,
    };
    while let Ok(request) = server.next().await {
        match request {
            // A lock that waits holds up the connection's other requests,
            // as a waiting `fcntl` does.
            NodeRequest::Lock(q, r) => {
                let locked = connection.lock(q).await;
                let _ = r.reply(server.server(), locked.map(|()| NodeLockResponse {}));
            }
            request => connection.handle(request, server.server()),
        }
    }
    connection.fs.close(connection.target, owner);
}

impl<F: Format> Connection<F> {
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
            R::SetAttributes(q, r) => {
                let fs = self.fs.clone();
                r.reply(s, fs.change(|| self.set_attributes(q)))
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
            R::Write(q, r) => {
                let fs = self.fs.clone();
                let written = fs.change(|| {
                    self.write(Some(q.offset), &q.data)
                        .map(|_| q.data.len() as u32)
                });
                r.reply(s, written.map(|written| NodeWriteResponse { written }))
            }
            R::Append(q, r) => {
                let fs = self.fs.clone();
                let written = fs.change(|| self.write(None, &q.data));
                r.reply(s, written.map(|offset| NodeAppendResponse { offset }))
            }
            R::WriteNext(q, r) => {
                let fs = self.fs.clone();
                r.reply(s, fs.change(|| self.write_next(&q.data)))
            }
            R::Truncate(q, r) => {
                let fs = self.fs.clone();
                let truncated = fs.change(|| self.truncate(q.size));
                r.reply(s, truncated.map(|()| NodeTruncateResponse {}))
            }
            R::Seek(q, r) => r.reply(s, self.seek(q.offset, q.origin)),
            R::Open(q, r) => {
                let changes =
                    q.flags.contains(OpenFlags::CREATE) || q.flags.contains(OpenFlags::TRUNCATE);
                let fs = self.fs.clone();
                match changes {
                    true => r.reply(s, fs.change(|| self.open(q))),
                    false => r.reply(s, self.open(q)),
                }
            }
            R::ReadDirectory(q, r) => r.reply(s, self.read_directory(q.cookie, q.attributes)),
            R::Unlink(q, r) => {
                let fs = self.fs.clone();
                let unlinked = fs.change(|| self.unlink(&q.name, q.directory));
                r.reply(s, unlinked.map(|()| NodeUnlinkResponse {}))
            }
            R::GetToken(_, r) => r.reply(s, self.token()),
            R::Rename(q, r) => {
                let fs = self.fs.clone();
                let renamed = fs.change(|| self.rename(q));
                r.reply(s, renamed.map(|()| NodeRenameResponse {}))
            }
            R::ReadLink(_, r) => r.reply(s, Err(Error::Invalid)),
            // What the volume cannot hold.
            R::CloneRange(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Link(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::CreateSymlink(_, r) => r.reply(s, Err(Error::NotSupported)),
            // What this server does not provide.
            R::GetMemoryObject(q, r) => r.reply(s, self.memory(q.flags)),
            R::GetRevocableMemoryObject(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Lock(_, r) => r.reply(s, Err(Error::Invalid)),
            R::Unlock(q, r) => r.reply(
                s,
                self.unlock(q.start, q.length)
                    .map(|()| NodeUnlockResponse {}),
            ),
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

    /// Checks that the connection may change the file system: it needs
    /// `WRITE`, and the volume must be writable.
    fn require_write(&self) -> Result<(), Error> {
        self.require(Rights::WRITE)?;
        match self.fs.read_only() {
            true => Err(Error::ReadOnly),
            false => Ok(()),
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

    fn set_attributes(
        &self,
        q: NodeSetAttributesRequest,
    ) -> Result<NodeSetAttributesResponse, Error> {
        if q.mask.contains(AttributeMask::MODE) {
            return Err(Error::NotSupported);
        }
        self.require_write()?;
        let Target::Node(id) = self.target else {
            return Err(Error::NotSupported);
        };
        let local = self.fs.clock.local();
        let accessed = q
            .mask
            .contains(AttributeMask::ACCESSED)
            .then(|| F::stamp(q.accessed, &local));
        let modified = q
            .mask
            .contains(AttributeMask::MODIFIED)
            .then(|| F::stamp(q.modified, &local));
        self.fs.with_node(id, |volume, node| {
            volume.set_times(node, accessed, modified)
        })?;
        Ok(NodeSetAttributesResponse {})
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

    /// Writes `data` at `offset`, or at the end without one, and returns
    /// where it went.
    fn write(&self, offset: Option<u64>, data: &[u8]) -> Result<u64, Error> {
        self.require_write()?;
        let id = self.file()?;
        let now = self.fs.now();
        let written = self
            .fs
            .with_node(id, |volume, node| {
                let at = offset.unwrap_or(F::size(node));
                volume.write(node, at, data, now).map(|()| at)
            })
            .map_err(|e| too_big(offset, data.len(), F::FILE_MAX, e));
        self.fs.changed(id);
        written
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
        self.require_write()?;
        if size > F::FILE_MAX {
            return Err(Error::TooBig);
        }
        let id = self.file()?;
        let now = self.fs.now();
        let truncated = self
            .fs
            .with_node(id, |volume, node| volume.truncate(node, size, now));
        self.fs.changed(id);
        truncated
    }

    fn seek(&mut self, offset: i64, origin: SeekOrigin) -> Result<NodeSeekResponse, Error> {
        let id = self.file().map_err(|_| Error::Invalid)?;
        let base = match origin {
            SeekOrigin::Start => 0,
            SeekOrigin::Current => self.cursor,
            SeekOrigin::End => self.fs.with_node(id, |_, node| Ok(F::size(node)))?,
            SeekOrigin::Unknown(_) => return Err(Error::Invalid),
        };
        let position = i128::from(base) + i128::from(offset);
        self.cursor = u64::try_from(position).map_err(|_| Error::Invalid)?;
        Ok(NodeSeekResponse {
            position: self.cursor,
        })
    }

    /// A memory object with the file's contents: with `PRIVATE` a copy of
    /// its own, and otherwise the file's paged object, which is never
    /// writable.
    fn memory(&self, flags: MemoryFlags) -> Result<NodeGetMemoryObjectResponse, Error> {
        self.require(Rights::READ)?;
        let id = self.file()?;
        let execute = flags.contains(MemoryFlags::EXECUTE);
        if execute {
            self.require(Rights::EXECUTE)?;
        }
        let write = flags.contains(MemoryFlags::WRITE);
        let memory = if flags.contains(MemoryFlags::PRIVATE) {
            self.fs.private_memory(id, memory::rights(write, execute))?
        } else if write {
            return Err(Error::NotSupported);
        } else {
            self.fs.paged_memory(id, memory::rights(false, execute))?
        };
        Ok(NodeGetMemoryObjectResponse { memory })
    }

    /// Takes a byte-range lock of the file, waiting for it when asked to
    /// and otherwise failing with `WOULD_BLOCK` when it conflicts. A
    /// shared lock needs `READ`, and an exclusive one `WRITE`, as
    /// `fcntl` does.
    async fn lock(&self, q: NodeLockRequest) -> Result<(), Error> {
        let id = self.file()?;
        let exclusive = match q.kind {
            LockKind::Shared => false,
            LockKind::Exclusive => true,
            LockKind::Unknown(_) => return Err(Error::Invalid),
        };
        self.require(if exclusive {
            Rights::WRITE
        } else {
            Rights::READ
        })?;
        let range = range(q.start, q.length)?;
        poll_fn(|cx| {
            let waker = q.wait.then_some(cx.waker());
            match self.fs.try_lock(id, self.owner, exclusive, range, waker) {
                Ok(true) => Poll::Ready(Ok(())),
                Ok(false) if q.wait => Poll::Pending,
                Ok(false) => Poll::Ready(Err(Error::WouldBlock)),
                Err(e) => Poll::Ready(Err(e)),
            }
        })
        .await
    }

    fn unlock(&self, start: u64, length: u64) -> Result<(), Error> {
        let id = self.file()?;
        self.fs.unlock(id, self.owner, range(start, length)?);
        Ok(())
    }

    fn open(&self, q: NodeOpenRequest) -> Result<NodeOpenResponse, Error> {
        self.require(q.rights)?;
        if q.rights.contains(Rights::WRITE) && self.fs.read_only() {
            return Err(Error::ReadOnly);
        }
        let mut dir = self.fs.dir(self.target)?;
        let (parts, trailing) = components(&q.path)?;
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
            dir = volume.as_dir(&node).ok_or(Error::NotDirectory)?;
        }
        let want_dir = q.flags.contains(OpenFlags::DIRECTORY) || trailing;
        let found = self.fs.volume.borrow_mut().find(dir, last);
        let node = match found {
            Ok(_) if q.flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUSIVE) => {
                return Err(Error::Exists);
            }
            Ok(node) if want_dir && !F::is_directory(&node) => return Err(Error::NotDirectory),
            Ok(node) => node,
            Err(mikros_fat::Error::NotFound) if q.flags.contains(OpenFlags::CREATE) => {
                self.require_write()?;
                let directory = q.flags.contains(OpenFlags::DIRECTORY);
                let now = self.fs.now();
                self.fs
                    .volume
                    .borrow_mut()
                    .create(dir, last, directory, now)
                    .map_err(fs::error)?
            }
            Err(e) => return Err(fs::error(e)),
        };
        let r#type = if F::is_directory(&node) {
            NodeType::Directory
        } else {
            NodeType::File
        };
        let target = self.fs.target(node);
        if q.flags.contains(OpenFlags::TRUNCATE) && r#type == NodeType::File {
            self.require_write()?;
            let Target::Node(id) = target else {
                return Err(Error::Io);
            };
            let now = self.fs.now();
            let truncated = self
                .fs
                .with_node(id, |volume, node| volume.truncate(node, 0, now));
            self.fs.changed(id);
            truncated?;
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
        let local = self.fs.clock.local();
        let mut volume = self.fs.volume.borrow_mut();
        let (nodes, next) = volume.list(dir, from, ENTRIES_MAX).map_err(fs::error)?;
        let mut entries = Vec::with_capacity(nodes.len());
        for node in nodes {
            let attrs = fs::node_attributes(&mut *volume, &node, &local).map_err(fs::error)?;
            entries.push(DirEntry {
                name: F::name(&node).into(),
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

    fn unlink(&self, name: &str, directory: bool) -> Result<(), Error> {
        self.require_write()?;
        let dir = self.fs.dir(self.target)?;
        check_component(name)?;
        let mut volume = self.fs.volume.borrow_mut();
        let node = volume.find(dir, name).map_err(fs::error)?;
        volume.remove(dir, name, directory).map_err(fs::error)?;
        self.fs.removed(F::place(&node));
        Ok(())
    }

    fn token(&self) -> Result<NodeGetTokenResponse, Error> {
        self.require(Rights::WRITE)?;
        let dir = self.fs.dir(self.target)?;
        Ok(NodeGetTokenResponse {
            token: self.fs.token(dir)?,
        })
    }

    fn rename(&self, q: NodeRenameRequest) -> Result<(), Error> {
        self.require_write()?;
        let from = self.fs.dir(self.target)?;
        check_component(&q.name)?;
        check_component(&q.new_name)?;
        let to = self.fs.token_dir(&q.target).ok_or(Error::CrossDevice)?;
        let mut volume = self.fs.volume.borrow_mut();
        let node = volume.find(from, &q.name).map_err(fs::error)?;
        // An entry that the rename replaces is gone; its slots may take
        // the moved entry.
        if let Ok(existing) = volume.find(to, &q.new_name)
            && F::place(&existing) != F::place(&node)
        {
            let moved = volume
                .rename(from, &q.name, to, &q.new_name)
                .map_err(fs::error)?;
            self.fs.removed(F::place(&existing));
            self.fs.moved(F::place(&node), &moved);
            return Ok(());
        }
        let moved = volume
            .rename(from, &q.name, to, &q.new_name)
            .map_err(fs::error)?;
        self.fs.moved(F::place(&node), &moved);
        Ok(())
    }
}

/// `TOO_BIG` for a write that would take the file past `max` bytes, the
/// most a file holds, and `e` otherwise.
fn too_big(offset: Option<u64>, len: usize, max: u64, e: Error) -> Error {
    let past = offset.is_some_and(|at| at.saturating_add(len as u64) > max);
    if past && e == Error::NoSpace {
        Error::TooBig
    } else {
        e
    }
}

/// The range of a lock from `start` with `length` bytes, 0 meaning to the
/// end of the file, `end` excluded.
fn range(start: u64, length: u64) -> Result<(u64, u64), Error> {
    match length {
        0 => Ok((start, u64::MAX)),
        n => Ok((start, start.checked_add(n).ok_or(Error::Invalid)?)),
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

/// Checks a name that `Unlink` or `Rename` gets.
fn check_component(name: &str) -> Result<(), Error> {
    match name {
        "" | "." | ".." => Err(Error::Invalid),
        n if n.contains('/') => Err(Error::Invalid),
        n if n.len() > NAME_MAX => Err(Error::NameTooLong),
        _ => Ok(()),
    }
}
