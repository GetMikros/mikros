//! The file system's tree, the connections open to its nodes, and the
//! `Filesystem` service.

use crate::file::File;
use crate::node;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use core::cell::RefCell;
use mikros_memfs::{Id, ROOT, Tree};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemAsyncServer, FilesystemGetRootResponse, FilesystemInfo,
    FilesystemQueryFilesystemResponse, FilesystemRequest, Node, Rights,
};
use mikros_rt::sys;

const PAGE: u32 = 4096;

pub type Nodes = Tree<File, ()>;

pub struct Fs {
    pub tree: RefCell<Nodes>,
    /// How many connections each node has.
    refs: RefCell<BTreeMap<Id, usize>>,
    pub reactor: Reactor,
    pub spawner: Spawner,
}

/// The protocol's error for the tree's.
pub fn error(e: mikros_memfs::Error) -> Error {
    use mikros_memfs::Error as E;
    match e {
        E::NotFound => Error::NotFound,
        E::Exists => Error::Exists,
        E::NotDirectory => Error::NotDirectory,
        E::IsDirectory => Error::IsDirectory,
        E::NotEmpty => Error::NotEmpty,
        E::Invalid => Error::Invalid,
        E::AccessDenied => Error::AccessDenied,
        E::NameTooLong => Error::NameTooLong,
        E::Loop => Error::Loop,
        E::NotSupported => Error::NotSupported,
    }
}

/// The time now, in nanoseconds since 1970 in UTC: the time since boot,
/// as the time service starts it without a real-time clock.
pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

impl Fs {
    pub fn new(reactor: Reactor, spawner: Spawner) -> Fs {
        Fs {
            tree: RefCell::new(Tree::new(0o755, 0, now())),
            refs: RefCell::new(BTreeMap::new()),
            reactor,
            spawner,
        }
    }

    /// Serves `end` as a connection to `node` with `rights` for the user
    /// `user`, writing at the end when `append`.
    pub fn serve(
        self: &Rc<Self>,
        end: ServerEnd<Node>,
        node: Id,
        rights: Rights,
        user: u32,
        append: bool,
    ) {
        *self.refs.borrow_mut().entry(node).or_default() += 1;
        let task = node::serve(self.clone(), end, node, rights, user, append);
        self.spawner.spawn(task);
    }

    /// Ends a connection's hold on `node`, which goes once it has no
    /// name and no connection left.
    pub fn close(&self, node: Id) {
        let mut refs = self.refs.borrow_mut();
        if let Some(count) = refs.get_mut(&node) {
            *count -= 1;
            if *count == 0 {
                refs.remove(&node);
                drop(refs);
                self.tree.borrow_mut().forget(node);
            }
        }
    }

    /// Lets go of `node` after its last name went, unless a connection
    /// still holds it.
    pub fn unnamed(&self, node: Id) {
        if !self.refs.borrow().contains_key(&node) {
            self.tree.borrow_mut().forget(node);
        }
    }

    pub fn info(&self) -> FilesystemInfo {
        FilesystemInfo {
            r#type: "tmpfs".into(),
            block_size: PAGE,
            total_bytes: 0,
            free_bytes: 0,
            read_only: false,
        }
    }
}

/// Serves `Filesystem` on `end` for the user `user`: `GetRoot` serves a
/// connection to the root directory.
pub async fn serve_filesystem(fs: Rc<Fs>, end: ServerEnd<Filesystem>, user: u32) {
    let server = FilesystemAsyncServer::new(end, fs.reactor.clone());
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            FilesystemRequest::GetRoot(q, r) => {
                fs.serve(q.node, ROOT, q.rights, user, false);
                r.reply(s, Ok(FilesystemGetRootResponse {}))
            }
            FilesystemRequest::QueryFilesystem(_, r) => {
                r.reply(s, Ok(FilesystemQueryFilesystemResponse { info: fs.info() }))
            }
        };
    }
}
