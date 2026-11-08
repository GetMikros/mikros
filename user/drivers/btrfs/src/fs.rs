// SPDX-License-Identifier: GPL-2.0
//! The served file system: the nodes that connections are open to, which
//! connections to the same node share, the tokens of directories, and
//! the `Filesystem` service
//! (MIKSPEC-0049.R13), whose root is the top-level subvolume
//! (MIKSPEC-0053.R4).

use crate::node;
use alloc::collections::BTreeMap;
use alloc::rc::{Rc, Weak};
use core::cell::RefCell;
use mikros_btrfs::Mount;
use mikros_btrfs::node::{
    Attributes as BtrfsAttributes, MODE_DIRECTORY, MODE_SYMLINK, MODE_TYPE, Node as BtrfsNode, Time,
};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::{
    Attributes, Error, Filesystem, FilesystemAsyncServer, FilesystemGetRootResponse,
    FilesystemInfo, FilesystemQueryFilesystemResponse, FilesystemRequest, Node, NodeType, Rights,
};
use mikros_rt::interfaces::mikros::service::{Provider, ProviderAsyncServer, ProviderRequest};

/// The protocol of the published service.
const PROTOCOL: &str = "device.filesystem";

const NS: i64 = 1_000_000_000;

/// What tells nodes apart: the device of their subvolume and their inode
/// number.
pub type Key = (u32, u64);

/// A node that connections are open to.
pub struct Open {
    pub node: BtrfsNode<'static>,
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
    opens: RefCell<BTreeMap<Key, Weak<Open>>>,
    /// The directories that tokens name, by the token's object ID.
    pub(crate) tokens: RefCell<BTreeMap<u64, Rc<Open>>>,
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
        let root = Rc::new(Open { node });
        let opens = BTreeMap::from([(key, Rc::downgrade(&root))]);
        Ok(Fs {
            mount,
            root,
            opens: RefCell::new(opens),
            tokens: RefCell::new(BTreeMap::new()),
            read_only,
            reactor,
            spawner,
        })
    }

    /// The open node for `node`: the one that connections to the same
    /// node share, if there is one.
    pub fn open(&self, node: BtrfsNode<'static>) -> Result<Rc<Open>, Error> {
        let key = key(&node)?;
        let mut opens = self.opens.borrow_mut();
        if let Some(open) = opens.get(&key).and_then(Weak::upgrade) {
            return Ok(open);
        }
        let open = Rc::new(Open { node });
        opens.insert(key, Rc::downgrade(&open));
        Ok(open)
    }

    /// Serves `end` as a connection to `open` with `rights` for the user
    /// `user`, writing at the end when `append`.
    pub fn serve(
        self: &Rc<Self>,
        end: ServerEnd<Node>,
        open: Rc<Open>,
        rights: Rights,
        user: u32,
        append: bool,
    ) {
        let task = node::serve(self.clone(), end, open, rights, user, append);
        self.spawner.spawn(task);
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
                    fs.serve(q.node, fs.root.clone(), q.rights, user, false);
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
