// SPDX-License-Identifier: GPL-2.0
//! The served file system: the nodes that connections are open to, which
//! connections to the same node share, the tokens of directories, the
//! byte-range locks and watches of connections, and the `Filesystem`
//! service (MIKSPEC-0049.R13). Its root is the top-level subvolume with
//! an entry `usr` of its own, the root of the system version that
//! `@system/current` names when the file system is mounted
//! (MIKSPEC-0053.R4, MIKSPEC-0053.R9).

use crate::change::koids;
use crate::lock::Lock;
use crate::node;
use crate::share::Shares;
use crate::watch::{Watches, event};
use alloc::collections::BTreeMap;
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::task::Waker;
use mikros_btrfs::Mount;
use mikros_btrfs::node::{
    Attributes as BtrfsAttributes, MODE_DIRECTORY, MODE_SYMLINK, MODE_TYPE, Node as BtrfsNode, Time,
};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::{
    Attributes, Error, Filesystem, FilesystemAsyncServer, FilesystemGetRootResponse,
    FilesystemInfo, FilesystemQueryFilesystemResponse, FilesystemRequest, Node, NodeType, Rights,
    WatchKind,
};
use mikros_rt::interfaces::mikros::service::{Provider, ProviderAsyncServer, ProviderRequest};

/// The protocol of the published service.
const PROTOCOL: &str = "device.filesystem";

const NS: i64 = 1_000_000_000;
/// The entry of the root that the server adds, and where its target is.
pub const USR: &str = "usr";
const SYSTEM: &[u8] = b"@system";
const CURRENT: &[u8] = b"current";

/// What tells nodes apart: the device of their subvolume and their inode
/// number.
pub type Key = (u32, u64);

/// A node that connections are open to.
pub struct Open {
    pub node: BtrfsNode<'static>,
    pub key: Key,
    /// The directories and names that the node was found under, which the
    /// watches of those directories hear of its changes by.
    names: RefCell<Vec<(Key, String)>>,
    pub(crate) locks: RefCell<Vec<Lock>>,
    pub(crate) waiting: RefCell<Vec<Waker>>,
}

impl Open {
    pub fn attributes(&self) -> Result<BtrfsAttributes, Error> {
        self.node.attributes().map_err(error)
    }

    pub fn is_directory(&self) -> Result<bool, Error> {
        Ok(self.attributes()?.mode & MODE_TYPE == MODE_DIRECTORY)
    }
}

pub struct Fs {
    pub mount: &'static Mount,
    pub root: Rc<Open>,
    /// The root of the current system version, which the root's `usr`
    /// names; none when there is no `@system/current`.
    pub usr: Option<Rc<Open>>,
    opens: RefCell<BTreeMap<Key, Weak<Open>>>,
    /// The directories that tokens name, by the token's object ID.
    pub(crate) tokens: RefCell<BTreeMap<u64, Rc<Open>>>,
    /// The nodes and rights of the connections being served, by the
    /// object ID of their server end, which `CloneRange` finds its
    /// source's by.
    pub(crate) connections: RefCell<BTreeMap<u64, (Rc<Open>, Rights)>>,
    pub watches: Watches,
    /// The files that clients have memory objects of.
    pub shares: Shares,
    next_owner: Cell<u64>,
    pub read_only: bool,
    pub reactor: Reactor,
    pub spawner: Spawner,
}

/// The protocol's error for a negative errno of the shim.
pub fn error(errno: i32) -> Error {
    match -errno {
        2 => Error::NotFound,
        17 => Error::Exists,
        20 => Error::NotDirectory,
        21 => Error::IsDirectory,
        39 => Error::NotEmpty,
        28 | 122 => Error::NoSpace,
        30 => Error::ReadOnly,
        1 | 13 => Error::AccessDenied,
        22 => Error::Invalid,
        36 => Error::NameTooLong,
        18 => Error::CrossDevice,
        40 => Error::Loop,
        95 | 25 | 38 => Error::NotSupported,
        16 | 26 => Error::Busy,
        27 => Error::TooBig,
        11 => Error::WouldBlock,
        _ => Error::Io,
    }
}

/// A time of the shim's attributes in nanoseconds since 1970.
pub fn utc(time: Time) -> i64 {
    time.seconds
        .saturating_mul(NS)
        .saturating_add(i64::from(time.nanoseconds))
}

/// The shim's time for nanoseconds since 1970.
pub fn time(utc: i64) -> Time {
    Time::new(utc.div_euclid(NS), utc.rem_euclid(NS) as u32)
}

/// The protocol's type of a node of `mode`.
pub fn node_type(mode: u32) -> NodeType {
    match mode & MODE_TYPE {
        MODE_DIRECTORY => NodeType::Directory,
        MODE_SYMLINK => NodeType::Symlink,
        0o140000 => NodeType::Socket,
        _ => NodeType::File,
    }
}

/// The protocol's attributes for the shim's.
pub fn attributes(a: &BtrfsAttributes) -> Attributes {
    Attributes {
        r#type: node_type(a.mode),
        mode: a.mode & 0o7777,
        owner: a.owner,
        size: a.size,
        allocated: a.allocated,
        links: a.links,
        inode: a.inode,
        created: utc(a.created),
        modified: utc(a.modified),
        accessed: utc(a.accessed),
        changed: utc(a.changed),
    }
}

impl Fs {
    pub fn new(
        mount: &'static Mount,
        read_only: bool,
        reactor: Reactor,
        spawner: Spawner,
    ) -> Result<Fs, Error> {
        let node = mount.root().map_err(error)?;
        let key = key(&node)?;
        let root = Rc::new(Open::new(node, key));
        let mut opens = BTreeMap::from([(key, Rc::downgrade(&root))]);
        let usr = current_version(&root.node).map(|node| {
            let usr = Rc::new(Open::new(node.0, node.1));
            usr.names.borrow_mut().push((key, String::from(USR)));
            opens.insert(usr.key, Rc::downgrade(&usr));
            usr
        });
        Ok(Fs {
            mount,
            root,
            usr,
            opens: RefCell::new(opens),
            tokens: RefCell::new(BTreeMap::new()),
            connections: RefCell::new(BTreeMap::new()),
            watches: Watches::default(),
            shares: Shares::default(),
            next_owner: Cell::new(1),
            read_only,
            reactor,
            spawner,
        })
    }

    /// Whether `name` in `dir` is the root's `usr`, which the server
    /// keeps from being created, removed or renamed.
    pub fn is_usr(&self, dir: &Open, name: &str) -> bool {
        dir.key == self.root.key && name == USR
    }

    /// The open node for `node`, found as `name` in `dir`: the one that
    /// connections to the same node share, if there is one.
    pub fn open(&self, node: BtrfsNode<'static>, dir: Key, name: &str) -> Result<Rc<Open>, Error> {
        let key = key(&node)?;
        let mut opens = self.opens.borrow_mut();
        let open = match opens.get(&key).and_then(Weak::upgrade) {
            Some(open) => open,
            None => {
                let open = Rc::new(Open::new(node, key));
                opens.insert(key, Rc::downgrade(&open));
                open
            }
        };
        drop(opens);
        let mut names = open.names.borrow_mut();
        if !names.iter().any(|(d, n)| *d == dir && n == name) {
            names.push((dir, String::from(name)));
        }
        drop(names);
        Ok(open)
    }

    /// Forgets that a node is named `name` in `dir`, which it no longer
    /// is, and gives it `to` instead if it was moved there.
    pub fn renamed(&self, dir: Key, name: &str, to: Option<(Key, &str)>) {
        for open in self.opens.borrow().values().filter_map(Weak::upgrade) {
            let mut names = open.names.borrow_mut();
            if let Some(at) = names.iter().position(|(d, n)| *d == dir && n == name) {
                match to {
                    Some((d, n)) => names[at] = (d, String::from(n)),
                    None => {
                        names.swap_remove(at);
                    }
                }
            }
        }
    }

    /// Tells the watches of `open` and of the directories that name it
    /// that its contents or attributes changed, as `kind` says.
    pub fn changed(&self, open: &Open, kind: WatchKind) {
        self.watches.notify(open.key, &[event(kind, 0, "")]);
        let names = open.names.borrow().clone();
        for (dir, name) in names {
            self.watches.notify(dir, &[event(kind, 0, &name)]);
        }
    }

    /// Serves `end` as a connection to `open` with `rights` for the user
    /// `user`, writing at the end when `append`, and looking names up
    /// case-insensitively when `folded` (MIKSPEC-0071.R16).
    pub fn serve(
        self: &Rc<Self>,
        end: ServerEnd<Node>,
        open: Rc<Open>,
        rights: Rights,
        user: u32,
        append: bool,
        folded: bool,
    ) {
        // Known from now on, before the task runs, so that a request
        // that follows the one that made the connection finds it.
        let id = koids(end.handle()).map(|(id, _)| id);
        if let Some(id) = id {
            let connection = (open.clone(), rights);
            self.connections.borrow_mut().insert(id, connection);
        }
        let fs = self.clone();
        let task = node::serve(self.clone(), end, open, rights, user, append, folded);
        self.spawner.spawn(async move {
            task.await;
            if let Some(id) = id {
                fs.connections.borrow_mut().remove(&id);
            }
        });
    }

    /// A new owner of locks and watches, for a connection.
    pub fn owner(&self) -> u64 {
        let owner = self.next_owner.get();
        self.next_owner.set(owner + 1);
        owner
    }

    /// What `QueryFilesystem` reports.
    pub fn info(&self) -> Result<FilesystemInfo, Error> {
        let statfs = self.mount.statfs().map_err(error)?;
        Ok(FilesystemInfo {
            r#type: "btrfs".into(),
            block_size: statfs.block_size as u32,
            total_bytes: statfs.blocks * statfs.block_size,
            free_bytes: statfs.blocks_available * statfs.block_size,
            read_only: self.read_only,
        })
    }
}

impl Open {
    fn new(node: BtrfsNode<'static>, key: Key) -> Open {
        Open {
            node,
            key,
            names: RefCell::new(Vec::new()),
            locks: RefCell::new(Vec::new()),
            waiting: RefCell::new(Vec::new()),
        }
    }
}

/// The root of the system version that `@system/current` in `root`
/// names, and its key, when there is one.
fn current_version(root: &BtrfsNode<'static>) -> Option<(BtrfsNode<'static>, Key)> {
    let system = root.lookup(SYSTEM).ok()?;
    let current = system.lookup(CURRENT).ok()?;
    let mut target = [0u8; 256];
    let len = current.read_link(&mut target).ok()?;
    let name = &target[..len];
    if name.is_empty() || name.contains(&b'/') {
        return None;
    }
    let version = system.lookup(name).ok()?;
    let key = key(&version).ok()?;
    Some((version, key))
}

fn key(node: &BtrfsNode<'_>) -> Result<Key, Error> {
    let a = node.attributes().map_err(error)?;
    Ok((a.device, a.inode))
}

/// Serves the provider `end` of the published `filesystem` service until
/// it closes: each connection to `device.filesystem` gets `Filesystem`,
/// for the user that connected.
pub async fn serve_provider(fs: Rc<Fs>, end: ServerEnd<Provider>) {
    let server = ProviderAsyncServer::new(end, fs.reactor.clone());
    while let Ok(ProviderRequest::Connect(request)) = server.next().await {
        if request.protocol == PROTOCOL {
            let end = ServerEnd::<Filesystem>::new(request.server);
            let user = request.client.user;
            fs.spawner.spawn(serve_filesystem(fs.clone(), end, user));
        }
    }
}

/// Serves `Filesystem` on `end` for `user`. `GetRoot` asking for `WRITE`
/// on a read-only file system fails with `READ_ONLY`.
async fn serve_filesystem(fs: Rc<Fs>, end: ServerEnd<Filesystem>, user: u32) {
    let server = FilesystemAsyncServer::new(end, fs.reactor.clone());
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            FilesystemRequest::GetRoot(q, r) => {
                if q.rights.contains(Rights::WRITE) && fs.read_only {
                    r.reply(s, Err(Error::ReadOnly))
                } else {
                    fs.serve(q.node, fs.root.clone(), q.rights, user, false, false);
                    r.reply(s, Ok(FilesystemGetRootResponse {}))
                }
            }
            FilesystemRequest::QueryFilesystem(_, r) => r.reply(
                s,
                fs.info()
                    .map(|info| FilesystemQueryFilesystemResponse { info }),
            ),
        };
    }
}
