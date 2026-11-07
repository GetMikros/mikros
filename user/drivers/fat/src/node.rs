//! One connection to the root directory of the volume.

use crate::fs::{self, Fs};
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::*;

/// The most entries one directory read returns, the limit of its reply.
const ENTRIES_MAX: usize = 64;

struct Connection {
    fs: Rc<Fs>,
    rights: Rights,
}

/// Serves the connection on `end` to the root directory with `rights`
/// until the client closes it or sends an invalid message.
pub async fn serve(fs: Rc<Fs>, end: ServerEnd<Node>, rights: Rights) {
    let server = NodeAsyncServer::new(end, fs.reactor.clone());
    let connection = Connection { fs, rights };
    while let Ok(request) = server.next().await {
        connection.handle(request, server.server());
    }
}

impl Connection {
    fn handle(&self, request: NodeRequest, s: &NodeServer) {
        use NodeRequest as R;
        // A reply that cannot be sent means that the client went away,
        // which the next read finds.
        let _ = match request {
            R::GetAttributes(_, r) => {
                let attributes = self.fs.root_attributes();
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
            R::ReadDirectory(q, r) => r.reply(s, self.read_directory(q.cookie, q.attributes)),
            R::Read(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::ReadNext(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::Seek(_, r) => r.reply(s, Err(Error::Invalid)),
            R::ReadLink(_, r) => r.reply(s, Err(Error::Invalid)),
            // Opening what the root holds is not there yet.
            R::Open(_, r) => r.reply(s, Err(Error::NotSupported)),
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

    fn clone(&self, q: NodeCloneRequest) -> Result<NodeCloneResponse, Error> {
        self.require(q.rights)?;
        self.fs.serve(q.node, q.rights);
        Ok(NodeCloneResponse {})
    }

    fn read_directory(
        &self,
        cookie: u64,
        attributes: bool,
    ) -> Result<NodeReadDirectoryResponse, Error> {
        self.require(Rights::READ)?;
        let Ok(from) = u32::try_from(cookie) else {
            return Ok(NodeReadDirectoryResponse {
                entries: Vec::new(),
                next: cookie,
            });
        };
        let mut volume = self.fs.volume.borrow_mut();
        let root = volume.root();
        let (nodes, next) = volume.list(root, from, ENTRIES_MAX).map_err(fs::error)?;
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
