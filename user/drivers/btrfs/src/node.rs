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

pub(crate) struct Connection {
    pub(crate) fs: Rc<Fs>,
    pub(crate) open: Rc<Open>,
    pub(crate) rights: Rights,
    /// The user of the connection, whom new nodes belong to.
    pub(crate) user: u32,
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
    let connection = Connection {
        fs,
        open,
        rights,
        user,
        append,
    };
    while let Ok(request) = server.next().await {
        connection.handle(request, server.server());
    }
}

impl Connection {
    pub(crate) fn handle(&self, request: NodeRequest, s: &NodeServer) {
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
            R::ReadLink(_, r) => r.reply(s, self.read_link()),
            R::Open(q, r) => r.reply(s, self.open(q)),
            R::ReadDirectory(q, r) => r.reply(s, self.read_directory(q.cookie, q.attributes)),
            // What this server does not provide yet.
            R::SetAttributes(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Read(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::ReadNext(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Write(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Append(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::WriteNext(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Truncate(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Seek(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unlink(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::GetToken(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Rename(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Link(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::CreateSymlink(_, r) => r.reply(s, Err(Error::NotSupported)),
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

    pub(crate) fn directory(&self) -> Result<(), Error> {
        match self.open.is_directory()? {
            true => Ok(()),
            false => Err(Error::NotDirectory),
        }
    }
}
