// SPDX-License-Identifier: GPL-2.0
//! One connection to a node (MIKSPEC-0049.R1): its requests, which the
//! other modules do, through the nodes of the shim, which look names up
//! one at a time, so that the server resolves paths as MIKSPEC-0049.R4
//! and R5 have them.

use crate::fs::{self, Fs, Open, error};
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
}

/// Serves the connection on `end` to `open` until the client closes it
/// or sends an invalid message.
pub async fn serve(
    fs: Rc<Fs>,
    end: ServerEnd<Node>,
    open: Rc<Open>,
    rights: Rights,
    user: u32,
    append: bool,
) {
    let server = NodeAsyncServer::new(end, fs.reactor.clone());
    let mut connection = Connection {
        fs,
        open,
        rights,
        user,
        cursor: 0,
        append,
    };
    while let Ok(request) = server.next().await {
        connection.handle(request, server.server());
    }
}

impl Connection {
    pub(crate) fn handle(&mut self, request: NodeRequest, s: &NodeServer) {
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
            R::SetAttributes(q, r) => r.reply(s, self.set_attributes(q)),
            R::Clone(q, r) => r.reply(s, self.clone(q)),
            R::Sync(_, r) => r.reply(
                s,
                self.open
                    .node
                    .sync()
                    .map(|()| NodeSyncResponse {})
                    .map_err(error),
            ),
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
            R::Write(q, r) => r.reply(
                s,
                self.write(Some(q.offset), &q.data)
                    .map(|(written, _)| NodeWriteResponse { written }),
            ),
            R::Append(q, r) => r.reply(
                s,
                self.write(None, &q.data)
                    .map(|(_, offset)| NodeAppendResponse { offset }),
            ),
            R::WriteNext(q, r) => r.reply(s, self.write_next(&q.data)),
            R::Truncate(q, r) => {
                r.reply(s, self.truncate(q.size).map(|()| NodeTruncateResponse {}))
            }
            R::Seek(q, r) => r.reply(s, self.seek(q.offset, q.origin)),
            R::ReadLink(_, r) => r.reply(s, self.read_link()),
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
            R::CreateSymlink(q, r) => r.reply(
                s,
                self.create_symlink(&q.name, &q.target)
                    .map(|()| NodeCreateSymlinkResponse {}),
            ),
            // What this server does not provide yet.
            R::Lock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unlock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Watch(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unwatch(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::GetMemoryObject(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::GetRevocableMemoryObject(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::CloneRange(_, r) => r.reply(s, Err(Error::NotSupported)),
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
