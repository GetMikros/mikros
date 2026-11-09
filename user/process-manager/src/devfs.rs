//! The `/dev` directory of every process's namespace, served over the
//! `Node` protocol, one executor task per connection. It holds only the
//! file `null`, whose reads return no bytes and whose writes succeed and
//! discard every byte. The directory itself cannot be changed.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::*;

/// The name of the only file.
const NULL: &str = "null";
/// The block size reported.
const BLOCK: u32 = 512;

/// The two nodes.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Directory,
    Null,
}

/// What connections run on.
#[derive(Clone)]
pub struct Dev {
    reactor: Reactor,
    spawner: Spawner,
}

impl Dev {
    pub fn new(reactor: Reactor, spawner: Spawner) -> Dev {
        Dev { reactor, spawner }
    }

    /// A new connection to the directory with `READ` and `WRITE`, which
    /// files opened through it may have.
    pub fn directory(&self) -> Result<ClientEnd<Node>, Status> {
        let (client, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
        self.serve(server, Kind::Directory, Rights::READ | Rights::WRITE);
        Ok(client)
    }

    fn serve(&self, end: ServerEnd<Node>, kind: Kind, rights: Rights) {
        let dev = self.clone();
        self.spawner
            .spawn(async move { serve(dev, end, kind, rights).await });
    }
}

struct Connection {
    dev: Dev,
    kind: Kind,
    rights: Rights,
}

/// Serves the connection on `end` to `kind` with `rights` until the
/// client closes it or sends an invalid message.
async fn serve(dev: Dev, end: ServerEnd<Node>, kind: Kind, rights: Rights) {
    let server = NodeAsyncServer::new(end, dev.reactor.clone());
    let connection = Connection { dev, kind, rights };
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
            R::GetAttributes(_, r) => r.reply(
                s,
                Ok(NodeGetAttributesResponse {
                    attributes: attributes(self.kind),
                }),
            ),
            R::Clone(q, r) => r.reply(s, self.clone(q)),
            R::Sync(_, r) => r.reply(s, Ok(NodeSyncResponse {})),
            R::QueryFilesystem(_, r) => r.reply(
                s,
                Ok(NodeQueryFilesystemResponse {
                    info: FilesystemInfo {
                        r#type: "devfs".into(),
                        block_size: BLOCK,
                        total_bytes: 0,
                        free_bytes: 0,
                        read_only: true,
                        label: String::new(),
                    },
                }),
            ),
            R::Read(_, r) => r.reply(s, self.read().map(|data| NodeReadResponse { data })),
            R::ReadNext(_, r) => r.reply(s, self.read().map(|data| NodeReadNextResponse { data })),
            R::Write(q, r) => r.reply(
                s,
                self.write(&q.data)
                    .map(|written| NodeWriteResponse { written }),
            ),
            R::WriteNext(q, r) => r.reply(
                s,
                self.write(&q.data)
                    .map(|written| NodeWriteNextResponse { written }),
            ),
            R::Append(q, r) => r.reply(
                s,
                self.write(&q.data)
                    .map(|_| NodeAppendResponse { offset: 0 }),
            ),
            R::Truncate(_, r) => r.reply(s, self.write(&[]).map(|_| NodeTruncateResponse {})),
            R::Seek(_, r) => r.reply(s, self.seek()),
            R::Open(q, r) => r.reply(s, self.open(q)),
            R::ReadDirectory(q, r) => r.reply(s, self.read_directory(q.cookie, q.attributes)),
            R::ReadLink(_, r) => r.reply(s, Err(Error::Invalid)),
            R::GetMemoryObject(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Lock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unlock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Watch(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unwatch(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::GetRevocableMemoryObject(_, r) => r.reply(s, Err(Error::NotSupported)),
            // Everything that would change the directory or a node's
            // attributes.
            R::SetAttributes(_, r) => r.reply(s, Err(self.write_denied())),
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

    /// The error of a change that is not allowed: without `WRITE` the
    /// right is missing, and with it the directory is still read-only.
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

    fn clone(&self, q: NodeCloneRequest) -> Result<NodeCloneResponse, Error> {
        self.require(q.rights)?;
        self.dev.serve(q.node, self.kind, q.rights);
        Ok(NodeCloneResponse {})
    }

    /// No bytes, from `null`.
    fn read(&self) -> Result<Vec<u8>, Error> {
        self.require(Rights::READ)?;
        match self.kind {
            Kind::Null => Ok(Vec::new()),
            Kind::Directory => Err(Error::IsDirectory),
        }
    }

    /// Takes every byte of `data` for `null` and drops it.
    fn write(&self, data: &[u8]) -> Result<u32, Error> {
        if self.kind == Kind::Directory {
            return Err(Error::IsDirectory);
        }
        self.require(Rights::WRITE)?;
        Ok(data.len() as u32)
    }

    /// `null` has no size, so every position is its start.
    fn seek(&self) -> Result<NodeSeekResponse, Error> {
        match self.kind {
            Kind::Null => Ok(NodeSeekResponse { position: 0 }),
            Kind::Directory => Err(Error::Invalid),
        }
    }

    fn open(&self, q: NodeOpenRequest) -> Result<NodeOpenResponse, Error> {
        self.require(q.rights)?;
        if self.kind != Kind::Directory {
            return Err(Error::NotDirectory);
        }
        let mut target = Kind::Directory;
        for name in q.path.split('/').filter(|n| !n.is_empty() && *n != ".") {
            target = match (target, name) {
                (Kind::Directory, NULL) => Kind::Null,
                (Kind::Null, _) => return Err(Error::NotDirectory),
                (Kind::Directory, _) if q.flags.contains(OpenFlags::CREATE) => {
                    return Err(self.write_denied());
                }
                (Kind::Directory, _) => return Err(Error::NotFound),
            };
        }
        if q.flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUSIVE) {
            return Err(Error::Exists);
        }
        if target == Kind::Null && q.flags.contains(OpenFlags::DIRECTORY) {
            return Err(Error::NotDirectory);
        }
        // Truncating `null` changes nothing.
        if target == Kind::Directory && q.flags.contains(OpenFlags::TRUNCATE) {
            return Err(Error::IsDirectory);
        }
        self.dev.serve(q.node, target, q.rights);
        Ok(NodeOpenResponse {
            r#type: attributes(target).r#type,
            redirect: None,
        })
    }

    fn read_directory(
        &self,
        cookie: u64,
        attributes: bool,
    ) -> Result<NodeReadDirectoryResponse, Error> {
        self.require(Rights::READ)?;
        if self.kind != Kind::Directory {
            return Err(Error::NotDirectory);
        }
        let entries = match cookie {
            0 => {
                let attrs = self::attributes(Kind::Null);
                alloc::vec![DirEntry {
                    name: String::from(NULL),
                    r#type: attrs.r#type,
                    inode: attrs.inode,
                    attributes: attributes.then(|| Box::new(attrs)),
                }]
            }
            _ => Vec::new(),
        };
        let next = cookie + entries.len() as u64;
        Ok(NodeReadDirectoryResponse { entries, next })
    }
}

fn attributes(kind: Kind) -> Attributes {
    let (r#type, mode, links, inode) = match kind {
        Kind::Directory => (NodeType::Directory, 0o555, 2, 1),
        Kind::Null => (NodeType::File, 0o666, 1, 2),
    };
    Attributes {
        r#type,
        mode,
        owner: 0,
        size: 0,
        allocated: 0,
        links,
        inode,
        created: 0,
        modified: 0,
        accessed: 0,
        changed: 0,
    }
}
