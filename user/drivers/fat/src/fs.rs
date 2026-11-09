//! The served volume, the nodes that connections are open to, the
//! tokens of directories, and the `Filesystem` service.

use crate::format::Format;
use crate::node;
use crate::time::{self, Utc};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::task::Waker;
use mikros_fat::Stamp;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::fs::{
    Attributes, Error, Filesystem, FilesystemAsyncServer, FilesystemGetRootResponse,
    FilesystemInfo, FilesystemQueryFilesystemResponse, FilesystemRequest, Node, NodeType, Rights,
};
use mikros_rt::interfaces::mikros::service::{Provider, ProviderAsyncServer, ProviderRequest};
use mikros_rt::sys;

/// The protocol of the published service.
const PROTOCOL: &str = "device.filesystem";
const INFO_BASIC: u32 = 1;
/// The rights of a token: DUPLICATE, TRANSFER and INSPECT.
const TOKEN_RIGHTS: u32 = 1 << 0 | 1 << 1 | 1 << 9;

/// What a connection is open to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Root,
    /// A node in `Fs::nodes`.
    Node(u64),
}

/// A node that connections are open to: its entry as the volume has it
/// now, how many connections, whether it has been removed, the byte-range
/// locks of its connections, and the tasks that wait for one.
struct Open<N> {
    node: N,
    refs: usize,
    removed: bool,
    locks: Vec<Lock>,
    waiting: Vec<Waker>,
}

/// A byte-range lock: its owner, a connection, whether it is exclusive,
/// and its range, `end` excluded.
#[derive(Clone, Copy)]
struct Lock {
    owner: u64,
    exclusive: bool,
    start: u64,
    end: u64,
}

pub struct Fs<F: Format> {
    pub volume: RefCell<F>,
    nodes: RefCell<BTreeMap<u64, Open<F::Node>>>,
    next: Cell<u64>,
    /// The directories that tokens name, by the token's object ID.
    tokens: RefCell<BTreeMap<u64, F::Dir>>,
    /// The pager of the paged objects of files, and those objects with
    /// their sizes, by node.
    pub pager: OwnedHandle,
    pub paged: RefCell<BTreeMap<u64, (OwnedHandle, u64)>>,
    pub utc: Utc,
    /// Whether the partition's `cache` property is `quick-disconnect`
    /// (MIKSPEC-0025.R33), which has every change durable when its call
    /// returns (MIKSPEC-0049.R20).
    quick_disconnect: bool,
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

impl<F: Format> Fs<F> {
    pub fn new(
        volume: F,
        utc: Utc,
        pager: OwnedHandle,
        quick_disconnect: bool,
        reactor: Reactor,
        spawner: Spawner,
    ) -> Fs<F> {
        Fs {
            volume: RefCell::new(volume),
            nodes: RefCell::new(BTreeMap::new()),
            next: Cell::new(1),
            tokens: RefCell::new(BTreeMap::new()),
            pager,
            paged: RefCell::new(BTreeMap::new()),
            utc,
            quick_disconnect,
            reactor,
            spawner,
        }
    }

    /// Makes `call`, which may change the volume, with `quick-disconnect`
    /// as one change (MIKSPEC-0064.R14): the volume is marked as in use on
    /// stable storage before it, and once it returns, as the volume
    /// writes what changes through at once, the partition is flushed and
    /// the volume marked as shut down cleanly again, on stable storage. A
    /// marking or flush that fails fails the call.
    pub fn change<T>(&self, call: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
        if !self.quick_disconnect {
            return call();
        }
        self.volume.borrow_mut().begin_change().map_err(error)?;
        let result = call();
        self.volume.borrow_mut().end_change().map_err(error)?;
        result
    }

    pub fn read_only(&self) -> bool {
        self.volume.borrow().read_only()
    }

    /// The local time now, for the stamps of changes.
    pub fn now(&self) -> F::Stamp {
        F::stamp(self.utc.now())
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

    /// Ends the hold of a connection, the lock owner `owner`, on
    /// `target`, releasing its locks.
    pub fn close(&self, target: Target, owner: u64) {
        let Target::Node(id) = target else {
            return;
        };
        self.unlock(id, owner, (0, u64::MAX));
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
    pub fn target(&self, node: F::Node) -> Target {
        let mut nodes = self.nodes.borrow_mut();
        let place = F::place(&node);
        let same = |o: &Open<F::Node>| !o.removed && F::place(&o.node) == place;
        if let Some((&id, _)) = nodes.iter().find(|(_, o)| same(o)) {
            return Target::Node(id);
        }
        let id = self.next.get();
        self.next.set(id + 1);
        nodes.insert(
            id,
            Open {
                node,
                refs: 0,
                removed: false,
                locks: Vec::new(),
                waiting: Vec::new(),
            },
        );
        Target::Node(id)
    }

    /// Runs `f` with the volume and the node of `id`. A removed node
    /// fails with `NOT_FOUND`.
    pub fn with_node<R>(
        &self,
        id: u64,
        f: impl FnOnce(&mut F, &mut F::Node) -> Result<R, mikros_fat::Error>,
    ) -> Result<R, Error> {
        let mut nodes = self.nodes.borrow_mut();
        let open = nodes.get_mut(&id).ok_or(Error::NotFound)?;
        if open.removed {
            return Err(Error::NotFound);
        }
        f(&mut self.volume.borrow_mut(), &mut open.node).map_err(error)
    }

    /// The directory that `target` is, or `NOT_DIRECTORY`.
    pub fn dir(&self, target: Target) -> Result<F::Dir, Error> {
        match target {
            Target::Root => Ok(self.volume.borrow().root()),
            Target::Node(id) => self
                .with_node(id, |volume, node| Ok(volume.as_dir(node)))?
                .ok_or(Error::NotDirectory),
        }
    }

    /// Whether `target` is a directory.
    pub fn is_directory(&self, target: Target) -> Result<bool, Error> {
        match target {
            Target::Root => Ok(true),
            Target::Node(id) => self.with_node(id, |_, node| Ok(F::is_directory(node))),
        }
    }

    /// Keeps node `id` for as long as the server runs.
    pub fn hold(&self, id: u64) {
        if let Some(open) = self.nodes.borrow_mut().get_mut(&id) {
            open.refs += 1;
        }
    }

    /// A new owner of locks, for a connection.
    pub fn owner(&self) -> u64 {
        let owner = self.next.get();
        self.next.set(owner + 1);
        owner
    }

    /// Takes the lock of `owner` on `start..end` of node `id`, replacing
    /// its own locks there, unless another owner's lock conflicts: then
    /// false, and `waker`, when given, is woken once a lock goes.
    pub fn try_lock(
        &self,
        id: u64,
        owner: u64,
        exclusive: bool,
        (start, end): (u64, u64),
        waker: Option<&Waker>,
    ) -> Result<bool, Error> {
        let mut nodes = self.nodes.borrow_mut();
        let open = nodes.get_mut(&id).ok_or(Error::NotFound)?;
        if open.removed {
            return Err(Error::NotFound);
        }
        let conflicts = open.locks.iter().any(|l| {
            l.owner != owner && l.start < end && start < l.end && (exclusive || l.exclusive)
        });
        if conflicts {
            if let Some(waker) = waker {
                open.waiting.push(waker.clone());
            }
            return Ok(false);
        }
        cut(&mut open.locks, owner, start, end);
        open.locks.push(Lock {
            owner,
            exclusive,
            start,
            end,
        });
        Ok(true)
    }

    /// Releases the locks of `owner` on `start..end` of node `id`.
    pub fn unlock(&self, id: u64, owner: u64, (start, end): (u64, u64)) {
        if let Some(open) = self.nodes.borrow_mut().get_mut(&id) {
            cut(&mut open.locks, owner, start, end);
            for waker in open.waiting.drain(..) {
                waker.wake();
            }
        }
    }

    /// Records that the entry at `place`, a directory and a slot there,
    /// has moved to `to`.
    pub fn moved(&self, place: (F::Dir, u32), to: &F::Node) {
        for open in self.nodes.borrow_mut().values_mut() {
            if !open.removed && F::place(&open.node) == place {
                open.node = to.clone();
            }
        }
    }

    /// Records that the entry at `place` is gone.
    pub fn removed(&self, place: (F::Dir, u32)) {
        for open in self.nodes.borrow_mut().values_mut() {
            if F::place(&open.node) == place {
                open.removed = true;
            }
        }
    }

    /// A new token that names `dir`.
    pub fn token(&self, dir: F::Dir) -> Result<OwnedHandle, Error> {
        let mut raw = 0;
        // SAFETY: the out pointer is valid for the call.
        if unsafe { sys::event_create(0, &mut raw) } != mikros_rt::OK {
            return Err(Error::Io);
        }
        // SAFETY: the kernel just created the handle for us.
        let event = unsafe { OwnedHandle::from_raw(raw) };
        let koid = koid(&event).ok_or(Error::Io)?;
        let mut token = 0;
        // SAFETY: the out pointer is valid for the call.
        if unsafe { sys::handle_duplicate(event.raw(), TOKEN_RIGHTS, &mut token) } != mikros_rt::OK
        {
            return Err(Error::Io);
        }
        // The server keeps the event, so that its object ID stays its own.
        core::mem::forget(event);
        self.tokens.borrow_mut().insert(koid, dir);
        // SAFETY: the kernel just created the handle for us.
        Ok(unsafe { OwnedHandle::from_raw(token) })
    }

    /// The directory that the token `handle` names, when it is one of
    /// this file system's.
    pub fn token_dir(&self, handle: &OwnedHandle) -> Option<F::Dir> {
        let koid = koid(handle)?;
        self.tokens.borrow().get(&koid).copied()
    }

    /// What `QueryFilesystem` reports.
    pub fn info(&self) -> Result<FilesystemInfo, Error> {
        let mut volume = self.volume.borrow_mut();
        let cluster = u64::from(volume.cluster_size());
        let free = u64::from(volume.free_clusters().map_err(error)?);
        Ok(FilesystemInfo {
            r#type: volume.kind().into(),
            block_size: volume.cluster_size(),
            total_bytes: u64::from(volume.clusters()) * cluster,
            free_bytes: free * cluster,
            read_only: volume.read_only(),
            label: volume.label().map_err(error)?.unwrap_or_default(),
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

    /// Writes what remains and marks the volume as shut down cleanly.
    pub fn unmount(&self) {
        if let Err(e) = self.volume.borrow_mut().unmount() {
            mikros_rt::log!(Error, "{}: unmounting failed: {e:?}", F::NAME);
        }
    }
}

/// Removes the parts of the locks of `owner` in `locks` that lie in
/// `start..end`, keeping what lies outside.
fn cut(locks: &mut Vec<Lock>, owner: u64, start: u64, end: u64) {
    let mut kept = Vec::with_capacity(locks.len());
    for lock in locks.drain(..) {
        if lock.owner != owner || lock.end <= start || end <= lock.start {
            kept.push(lock);
            continue;
        }
        if lock.start < start {
            kept.push(Lock { end: start, ..lock });
        }
        if end < lock.end {
            kept.push(Lock { start: end, ..lock });
        }
    }
    *locks = kept;
}

/// The attributes of the entry `node`.
pub fn node_attributes<F: Format>(
    volume: &mut F,
    node: &F::Node,
) -> Result<Attributes, mikros_fat::Error> {
    let allocated = volume.allocated(node)?;
    let directory = F::is_directory(node);
    let [created, modified, accessed] = F::times(node);
    Ok(Attributes {
        r#type: if directory {
            NodeType::Directory
        } else {
            NodeType::File
        },
        mode: if directory { 0o755 } else { 0o644 },
        owner: 0,
        size: if directory { allocated } else { F::size(node) },
        allocated,
        links: 1,
        inode: 2 + volume.entry_offset(node)? / 32,
        created,
        modified,
        accessed,
        changed: modified,
    })
}

/// The object ID of `handle`.
fn koid(handle: &OwnedHandle) -> Option<u64> {
    let mut record = [0u8; 24];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status = unsafe {
        sys::object_get_info(
            handle.raw(),
            INFO_BASIC,
            record.as_mut_ptr(),
            record.len() as u64,
            &mut len,
        )
    };
    let id = u64::from_le_bytes(record[..8].try_into().ok()?);
    (status == mikros_rt::OK).then_some(id)
}

/// Serves the provider `end` of the published `filesystem` service until
/// it closes: each connection to `device.filesystem` gets `Filesystem`.
pub async fn serve_provider<F: Format>(fs: Rc<Fs<F>>, end: ServerEnd<Provider>) {
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
async fn serve_filesystem<F: Format>(fs: Rc<Fs<F>>, end: ServerEnd<Filesystem>) {
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
