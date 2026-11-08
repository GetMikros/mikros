// SPDX-License-Identifier: GPL-2.0
//! One connection to a node (MIKSPEC-0049.R1): its requests, which the
//! other modules do, through the nodes of the shim, which look names up
//! one at a time, so that the server resolves paths as MIKSPEC-0049.R4
//! and R5 have them.

use crate::fs::{self, Fs, Open};
use alloc::rc::Rc;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::*;

/// The longest path component, in bytes.
pub(crate) const NAME_MAX: usize = 255;
/// The longest target of a symbolic link.
pub(crate) const TARGET_MAX: usize = 4096;
/// The permission bits of a mode.
pub(crate) const MODE_BITS: u32 = 0o7777;

pub(crate) struct Connection {
    pub(crate) fs: Rc<Fs>,
    pub(crate) open: Rc<Open>,
    pub(crate) rights: Rights,
    /// The user of the connection, whom new nodes belong to.
    pub(crate) user: u32,
    pub(crate) cursor: u64,
    pub(crate) append: bool,
    /// Whether the connection looks names up case-insensitively, as it was
    /// opened with `CASE_INSENSITIVE` (MIKSPEC-0071.R17).
    pub(crate) folded: bool,
    /// The owner of the connection's locks and watches.
    pub(crate) owner: u64,
}

/// Serves the connection on `end` to `open` until the client closes it
/// or sends an invalid message, and then ends its locks and watches.
pub async fn serve(
    fs: Rc<Fs>,
    end: ServerEnd<Node>,
    open: Rc<Open>,
    rights: Rights,
    user: u32,
    append: bool,
    folded: bool,
) {
    let server = NodeAsyncServer::new(end, fs.reactor.clone());
    let owner = fs.owner();
    let mut connection = Connection {
        fs,
        open,
        rights,
        user,
        cursor: 0,
        append,
        folded,
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
            request => connection.handle(request, server.server()).await,
        }
    }
    let Connection { fs, open, .. } = connection;
    fs.unlock(&open, owner, (0, u64::MAX));
    fs.watches.end(owner);
}

impl Connection {
    pub(crate) async fn handle(&mut self, request: NodeRequest, s: &NodeServer) {
        use NodeRequest as R;
        // A reply that cannot be sent means that the client went away,
        // which the next read finds.
        let _ = match request {
            R::GetAttributes(_, r) => r.reply(
                s,
                self.open.attributes().map(|a| NodeGetAttributesResponse {
                    attributes: fs::attributes(&a),
                }),
            ),
            R::SetAttributes(q, r) => {
                let set = self.set_attributes(q);
                r.reply(s, self.fs.durable(set).await)
            }
            R::Clone(q, r) => r.reply(s, self.clone(q)),
            R::Sync(_, r) => r.reply(s, self.sync().await.map(|()| NodeSyncResponse {})),
            R::QueryFilesystem(_, r) => r.reply(
                s,
                self.fs
                    .info()
                    .map(|info| NodeQueryFilesystemResponse { info }),
            ),
            R::Read(q, r) => r.reply(
                s,
                self.read(q.offset, q.count)
                    .await
                    .map(|data| NodeReadResponse { data }),
            ),
            R::ReadNext(q, r) => r.reply(s, self.read_next(q.count).await),
            R::Write(q, r) => {
                let written = self.write(Some(q.offset), q.data).await;
                r.reply(
                    s,
                    self.fs
                        .durable(written)
                        .await
                        .map(|(written, _)| NodeWriteResponse { written }),
                )
            }
            R::Append(q, r) => {
                let written = self.write(None, q.data).await;
                r.reply(
                    s,
                    self.fs
                        .durable(written)
                        .await
                        .map(|(_, offset)| NodeAppendResponse { offset }),
                )
            }
            R::WriteNext(q, r) => {
                let written = self.write_next(q.data).await;
                r.reply(s, self.fs.durable(written).await)
            }
            R::Truncate(q, r) => {
                let truncated = self.truncate(q.size);
                r.reply(
                    s,
                    self.fs
                        .durable(truncated)
                        .await
                        .map(|()| NodeTruncateResponse {}),
                )
            }
            R::Seek(q, r) => r.reply(s, self.seek(q.offset, q.origin)),
            R::ReadLink(_, r) => r.reply(s, self.read_link()),
            R::CloneRange(q, r) => {
                let cloned = self.clone_range(q);
                r.reply(
                    s,
                    self.fs
                        .durable(cloned)
                        .await
                        .map(|()| NodeCloneRangeResponse {}),
                )
            }
            R::Open(q, r) => {
                let changes =
                    q.flags.contains(OpenFlags::CREATE) || q.flags.contains(OpenFlags::TRUNCATE);
                let opened = self.open(q);
                match changes {
                    true => r.reply(s, self.fs.durable(opened).await),
                    false => r.reply(s, opened),
                }
            }
            R::ReadDirectory(q, r) => r.reply(s, self.read_directory(q.cookie, q.attributes)),
            R::Unlink(q, r) => {
                let unlinked = self.unlink(&q.name, q.directory);
                r.reply(
                    s,
                    self.fs
                        .durable(unlinked)
                        .await
                        .map(|()| NodeUnlinkResponse {}),
                )
            }
            R::GetToken(_, r) => r.reply(s, self.token()),
            R::Rename(q, r) => {
                let renamed = self.rename(q);
                r.reply(
                    s,
                    self.fs
                        .durable(renamed)
                        .await
                        .map(|()| NodeRenameResponse {}),
                )
            }
            R::Link(q, r) => {
                let linked = self.link(q);
                r.reply(
                    s,
                    self.fs.durable(linked).await.map(|()| NodeLinkResponse {}),
                )
            }
            R::CreateSymlink(q, r) => {
                let made = self.create_symlink(&q.name, &q.target);
                r.reply(
                    s,
                    self.fs
                        .durable(made)
                        .await
                        .map(|()| NodeCreateSymlinkResponse {}),
                )
            }
            R::Lock(_, r) => r.reply(s, Err(Error::Invalid)),
            R::Unlock(q, r) => r.reply(
                s,
                self.unlock(q.start, q.length)
                    .map(|()| NodeUnlockResponse {}),
            ),
            R::Watch(q, r) => r.reply(s, self.watch(q).map(|()| NodeWatchResponse {})),
            R::Unwatch(q, r) => r.reply(s, self.unwatch(q.cookie).map(|()| NodeUnwatchResponse {})),
            // What this server does not provide yet.
            R::GetMemoryObject(q, r) => r.reply(s, self.memory(q.flags)),
            R::GetRevocableMemoryObject(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::BindSocket(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::ConnectSocket(_, r) => r.reply(s, Err(Error::NotSupported)),
        };
    }

    pub(crate) fn require(&self, rights: Rights) -> Result<(), Error> {
        match self.rights.contains(rights) {
            true => Ok(()),
            false => Err(Error::AccessDenied),
        }
    }

    /// Checks that the connection may change the file system: it needs
    /// `WRITE`, and the file system must be writable.
    pub(crate) fn require_write(&self) -> Result<(), Error> {
        self.require(Rights::WRITE)?;
        match self.fs.read_only {
            true => Err(Error::ReadOnly),
            false => Ok(()),
        }
    }

    /// Fails for a directory, which cannot be read or written as a file.
    pub(crate) fn file(&self) -> Result<(), Error> {
        match self.open.is_directory()? {
            true => Err(Error::IsDirectory),
            false => Ok(()),
        }
    }

    pub(crate) fn directory(&self) -> Result<(), Error> {
        match self.open.is_directory()? {
            true => Ok(()),
            false => Err(Error::NotDirectory),
        }
    }
}
