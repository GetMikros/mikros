//! The served volume, the nodes that connections are open to, and the
//! `Filesystem` service.

use crate::device::BlockDevice;
use crate::node;
use crate::time;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use core::cell::{Cell, RefCell};
use mikros_fat::{Dir, FatType, Node as FatNode, Stamp, Volume};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::{
    Attributes, Error, Filesystem, FilesystemAsyncServer, FilesystemGetRootResponse,
    FilesystemInfo, FilesystemQueryFilesystemResponse, FilesystemRequest, Node, NodeType, Rights,
};
use mikros_rt::interfaces::mikros::service::{Provider, ProviderAsyncServer, ProviderRequest};

/// The protocol of the published service.
const PROTOCOL: &str = "device.filesystem";

/// What a connection is open to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Root,
    /// A node in `Fs::nodes`.
    Node(u64),
}

/// A node that connections are open to: its entry as the volume has it
/// now, and how many connections.
struct Open {
    node: FatNode,
    refs: usize,
}

pub struct Fs {
    pub volume: RefCell<Volume<BlockDevice>>,
    nodes: RefCell<BTreeMap<u64, Open>>,
    next: Cell<u64>,
    pub reactor: Reactor,
    pub spawner: Spawner,
}

/// The protocol's error for a volume's.
pub fn error(e: mikros_fat::Error) -> Error {
    use mikros_fat::Error as E;
    match e {
        E::Io => Error::Io,
        E::Invalid => Error::Invalid,
        E::NotFound => Error::NotFound,
        E::Exists => Error::Exists,
        E::NotDirectory => Error::NotDirectory,
        E::IsDirectory => Error::IsDirectory,
        E::NotEmpty => Error::NotEmpty,
        E::NoSpace => Error::NoSpace,
        E::ReadOnly => Error::ReadOnly,
        E::NameTooLong => Error::NameTooLong,
    }
}

impl Fs {
    pub fn new(volume: Volume<BlockDevice>, reactor: Reactor, spawner: Spawner) -> Fs {
        Fs {
            volume: RefCell::new(volume),
            nodes: RefCell::new(BTreeMap::new()),
            next: Cell::new(1),
            reactor,
            spawner,
        }
    }

    pub fn read_only(&self) -> bool {
        self.volume.borrow().read_only()
    }

    /// Serves `end` as a connection to `target` with `rights`, writing at
    /// the end when `append`. The connection holds `target` until it
    /// closes.
    pub fn serve(
        self: &Rc<Self>,
        end: ServerEnd<Node>,
        target: Target,
        rights: Rights,
        append: bool,
    ) {
        if let Target::Node(id) = target
            && let Some(open) = self.nodes.borrow_mut().get_mut(&id)
        {
            open.refs += 1;
        }
        let task = node::serve(self.clone(), end, target, rights, append);
        self.spawner.spawn(task);
    }

    /// Ends one connection's hold on `target`.
    pub fn close(&self, target: Target) {
        let Target::Node(id) = target else {
            return;
        };
        let mut nodes = self.nodes.borrow_mut();
        if let Some(open) = nodes.get_mut(&id) {
            open.refs -= 1;
            if open.refs == 0 {
                nodes.remove(&id);
            }
        }
    }

    /// The target for `node`, the one connections to the same entry
    /// share, which the caller then serves.
    pub fn target(&self, node: FatNode) -> Target {
        let mut nodes = self.nodes.borrow_mut();
        let same = |o: &Open| o.node.parent == node.parent && o.node.slot == node.slot;
        if let Some((&id, _)) = nodes.iter().find(|(_, o)| same(o)) {
            return Target::Node(id);
        }
        let id = self.next.get();
        self.next.set(id + 1);
        nodes.insert(id, Open { node, refs: 0 });
        Target::Node(id)
    }

    /// Runs `f` with the volume and the node of `id`.
    pub fn with_node<R>(
        &self,
        id: u64,
        f: impl FnOnce(&mut Volume<BlockDevice>, &mut FatNode) -> Result<R, mikros_fat::Error>,
    ) -> Result<R, Error> {
        let mut nodes = self.nodes.borrow_mut();
        let open = nodes.get_mut(&id).ok_or(Error::NotFound)?;
        f(&mut self.volume.borrow_mut(), &mut open.node).map_err(error)
    }

    /// The directory that `target` is, or `NOT_DIRECTORY`.
    pub fn dir(&self, target: Target) -> Result<Dir, Error> {
        let root = self.volume.borrow().root();
        match target {
            Target::Root => Ok(root),
            Target::Node(id) => self
                .with_node(id, |_, node| Ok(node.as_dir(root)))?
                .ok_or(Error::NotDirectory),
        }
    }

    /// Whether `target` is a directory.
    pub fn is_directory(&self, target: Target) -> Result<bool, Error> {
        match target {
            Target::Root => Ok(true),
            Target::Node(id) => self.with_node(id, |_, node| Ok(node.is_directory())),
        }
    }

    /// What `QueryFilesystem` reports.
    pub fn info(&self) -> Result<FilesystemInfo, Error> {
        let mut volume = self.volume.borrow_mut();
        let layout = volume.layout().clone();
        let cluster = u64::from(layout.cluster_size);
        let free = u64::from(volume.free_clusters().map_err(error)?);
        Ok(FilesystemInfo {
            r#type: match layout.fat_type {
                FatType::Fat12 => "fat12".into(),
                FatType::Fat16 => "fat16".into(),
                FatType::Fat32 => "fat32".into(),
            },
            block_size: layout.cluster_size,
            total_bytes: u64::from(layout.clusters) * cluster,
            free_bytes: free * cluster,
            read_only: volume.read_only(),
        })
    }

    /// The attributes of `target`.
    pub fn attributes(&self, target: Target) -> Result<Attributes, Error> {
        match target {
            Target::Root => {
                let mut volume = self.volume.borrow_mut();
                let root = volume.root();
                let allocated = volume.allocated_dir(root).map_err(error)?;
                let epoch = time::to_utc(Stamp::default());
                Ok(Attributes {
                    r#type: NodeType::Directory,
                    mode: 0o755,
                    owner: 0,
                    size: allocated,
                    allocated,
                    links: 1,
                    inode: 1,
                    created: epoch,
                    modified: epoch,
                    accessed: epoch,
                    changed: epoch,
                })
            }
            Target::Node(id) => self.with_node(id, |volume, node| node_attributes(volume, node)),
        }
    }
}

/// The attributes of the entry `node`.
pub fn node_attributes(
    volume: &mut Volume<BlockDevice>,
    node: &FatNode,
) -> Result<Attributes, mikros_fat::Error> {
    let allocated = volume.allocated(node.cluster())?;
    let directory = node.is_directory();
    let modified = time::to_utc(node.modified());
    Ok(Attributes {
        r#type: if directory {
            NodeType::Directory
        } else {
            NodeType::File
        },
        mode: if directory { 0o755 } else { 0o644 },
        owner: 0,
        size: if directory { allocated } else { node.size() },
        allocated,
        links: 1,
        inode: 2 + volume.entry_offset(node)? / 32,
        created: time::to_utc(node.created()),
        modified,
        accessed: time::to_utc(node.accessed()),
        changed: modified,
    })
}

/// Serves the provider `end` of the published `filesystem` service until
/// it closes: each connection to `device.filesystem` gets `Filesystem`.
pub async fn serve_provider(fs: Rc<Fs>, end: ServerEnd<Provider>) {
    let server = ProviderAsyncServer::new(end, fs.reactor.clone());
    while let Ok(ProviderRequest::Connect(request)) = server.next().await {
        if request.protocol == PROTOCOL {
            let end = ServerEnd::<Filesystem>::new(request.server);
            fs.spawner.spawn(serve_filesystem(fs.clone(), end));
        }
    }
}

/// Serves `Filesystem` on `end`. `GetRoot` asking for `WRITE` on a
/// read-only volume fails with `READ_ONLY`.
async fn serve_filesystem(fs: Rc<Fs>, end: ServerEnd<Filesystem>) {
    let server = FilesystemAsyncServer::new(end, fs.reactor.clone());
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            FilesystemRequest::GetRoot(q, r) => {
                if q.rights.contains(Rights::WRITE) && fs.read_only() {
                    r.reply(s, Err(Error::ReadOnly))
                } else {
                    fs.serve(q.node, Target::Root, q.rights, false);
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
