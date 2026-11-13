// SPDX-License-Identifier: GPL-2.0
//! Managing the subvolumes of the file system (MIKSPEC-0053.R8,
//! MIKSPEC-0053.R10): `mikros.fs.btrfs.Subvolumes`, which the server
//! publishes as a service of class `subvolumes` after its `filesystem`
//! one, so that the directory lets in only callers with the permission
//! `filesystem.manage`. Paths name subvolumes from the root of the
//! top-level subvolume, one component at a time, the server's own `usr`
//! left aside.

use crate::fs::{Fs, error};
use crate::watch::event;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_btrfs::node::Node;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::btrfs::*;
use mikros_rt::interfaces::mikros::fs::{Error, WatchKind};
use mikros_rt::interfaces::mikros::service::{Provider, ProviderAsyncServer, ProviderRequest};

/// The protocol of the published service.
const PROTOCOL: &str = "device.subvolumes";
/// The most subvolumes that one `List` returns, the limit of its reply.
const LIST_MAX: usize = 32;
/// The longest path, the limit of a request.
const PATH_MAX: usize = 1024;
const NAME_MAX: usize = 255;

/// Serves the provider `end` of the published `subvolumes` service until
/// it closes: each connection to `device.subvolumes` gets `Subvolumes`.
pub async fn serve_provider(fs: Rc<Fs>, end: ServerEnd<Provider>) {
    let server = ProviderAsyncServer::new(end, fs.reactor.clone());
    while let Ok(ProviderRequest::Connect(request)) = server.next().await {
        if request.protocol == PROTOCOL {
            let end = ServerEnd::<Subvolumes>::new(request.server);
            fs.spawner.spawn(serve(fs.clone(), end));
        }
    }
}

async fn serve(fs: Rc<Fs>, end: ServerEnd<Subvolumes>) {
    let server = SubvolumesAsyncServer::new(end, fs.reactor.clone());
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            SubvolumesRequest::Create(q, r) => r.reply(
                s,
                fs.durable(fs.create_subvolume(&q.path))
                    .await
                    .map(|()| SubvolumesCreateResponse {}),
            ),
            SubvolumesRequest::Snapshot(q, r) => r.reply(
                s,
                fs.durable(fs.snapshot(&q.source, &q.path, q.read_only))
                    .await
                    .map(|()| SubvolumesSnapshotResponse {}),
            ),
            SubvolumesRequest::Delete(q, r) => r.reply(
                s,
                fs.durable(fs.delete_subvolume(&q.path))
                    .await
                    .map(|()| SubvolumesDeleteResponse {}),
            ),
            SubvolumesRequest::SetReadOnly(q, r) => r.reply(
                s,
                fs.durable(fs.set_read_only(&q.path, q.read_only))
                    .await
                    .map(|()| SubvolumesSetReadOnlyResponse {}),
            ),
            SubvolumesRequest::List(q, r) => r.reply(s, fs.list_subvolumes(q.cursor)),
        };
    }
}

/// The components of `path`, which must not be empty, start with `/`, or
/// hold an empty component, `.` or `..`.
fn components(path: &str) -> Result<Vec<&str>, Error> {
    if path.is_empty() || path.len() > PATH_MAX || path.starts_with('/') {
        return Err(Error::Invalid);
    }
    let parts: Vec<&str> = path.split('/').collect();
    for part in &parts {
        match *part {
            "" | "." | ".." => return Err(Error::Invalid),
            p if p.len() > NAME_MAX => return Err(Error::NameTooLong),
            _ => {}
        }
    }
    Ok(parts)
}

impl Fs {
    /// The node that the components `parts` name from the root of the
    /// top-level subvolume.
    fn walk(&self, parts: &[&str]) -> Result<Node<'static>, Error> {
        let mut node = self.mount.root().map_err(error)?;
        for part in parts {
            node = node.lookup(part.as_bytes()).map_err(error)?;
        }
        Ok(node)
    }

    /// The directory that `path` names its last component in, and that
    /// component.
    fn parent<'p>(&self, path: &'p str) -> Result<(Node<'static>, &'p str), Error> {
        let parts = components(path)?;
        let (name, dirs) = parts.split_last().expect("a path has a component");
        Ok((self.walk(dirs)?, name))
    }

    /// Tells the watches of the directory `dir`, if a connection is open
    /// to it, of `name` made or removed, as `kind` says.
    fn entry_changed(&self, dir: &Node<'_>, name: &str, kind: WatchKind) {
        let Ok(a) = dir.attributes() else {
            return;
        };
        let key = (a.device, a.inode);
        if kind == WatchKind::Removed {
            self.renamed(key, name, None);
        }
        self.watches.notify(key, &[event(kind, 0, name)]);
    }

    fn create_subvolume(&self, path: &str) -> Result<(), Error> {
        self.writable()?;
        let (dir, name) = self.parent(path)?;
        dir.create_subvolume(name.as_bytes()).map_err(error)?;
        self.entry_changed(&dir, name, WatchKind::Created);
        Ok(())
    }

    fn snapshot(&self, source: &str, path: &str, read_only: bool) -> Result<(), Error> {
        self.writable()?;
        let source = self.walk(&components(source)?)?;
        let (dir, name) = self.parent(path)?;
        dir.snapshot(&source, name.as_bytes(), read_only)
            .map_err(error)?;
        self.entry_changed(&dir, name, WatchKind::Created);
        Ok(())
    }

    fn delete_subvolume(&self, path: &str) -> Result<(), Error> {
        self.writable()?;
        let (dir, name) = self.parent(path)?;
        dir.delete_subvolume(name.as_bytes()).map_err(error)?;
        self.entry_changed(&dir, name, WatchKind::Removed);
        Ok(())
    }

    /// Makes the subvolume at `path` read-only or writable. One with files
    /// that clients hold memory objects of that can never change stays
    /// read-only: making it writable fails with `BUSY` (MIKSPEC-0053.R11).
    fn set_read_only(&self, path: &str, read_only: bool) -> Result<(), Error> {
        self.writable()?;
        let root = self.walk(&components(path)?)?;
        if !read_only && self.shares.pins(root.attributes().map_err(error)?.device) {
            return Err(Error::Busy);
        }
        root.set_read_only(read_only).map_err(error)
    }

    /// The subvolumes after the ID `cursor`, as many as one reply holds,
    /// and the cursor to go on from.
    fn list_subvolumes(&self, mut cursor: u64) -> Result<SubvolumesListResponse, Error> {
        let root = self.mount.root().map_err(error)?;
        let mut subvolumes = Vec::new();
        root.subvolumes(&mut cursor, |path, id, read_only| {
            if subvolumes.len() == LIST_MAX {
                return false;
            }
            subvolumes.push(SubvolumeInfo {
                path: String::from_utf8_lossy(path).into_owned(),
                id,
                read_only,
            });
            true
        })
        .map_err(error)?;
        Ok(SubvolumesListResponse {
            subvolumes,
            next: cursor,
        })
    }

    /// Fails with `READ_ONLY` on a read-only mount.
    fn writable(&self) -> Result<(), Error> {
        match self.read_only {
            true => Err(Error::ReadOnly),
            false => Ok(()),
        }
    }
}
