//! The file system's tree, the connections open to its nodes, the
//! tokens of directories, and the `Filesystem` service.

use crate::file::File;
use crate::node;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use core::cell::RefCell;
use mikros_memfs::{Id, ROOT, Tree};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemAsyncServer, FilesystemGetRootResponse, FilesystemInfo,
    FilesystemQueryFilesystemResponse, FilesystemRequest, Node, Rights,
};
use mikros_rt::sys;

const INFO_BASIC: u32 = 1;
/// The rights of a token: DUPLICATE, TRANSFER and INSPECT.
const TOKEN_RIGHTS: u32 = 1 << 0 | 1 << 1 | 1 << 9;
const PAGE: u32 = 4096;

pub type Nodes = Tree<File, ()>;

pub struct Fs {
    pub tree: RefCell<Nodes>,
    /// How many connections each node has.
    refs: RefCell<BTreeMap<Id, usize>>,
    /// The directories that tokens name, by the token's object ID.
    tokens: RefCell<BTreeMap<u64, Id>>,
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
            tokens: RefCell::new(BTreeMap::new()),
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

    /// A new token that names the directory `dir`.
    pub fn token(&self, dir: Id) -> Result<OwnedHandle, Error> {
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
    /// this file system's and the directory is still there.
    pub fn token_dir(&self, handle: &OwnedHandle) -> Option<Id> {
        let dir = *self.tokens.borrow().get(&koid(handle)?)?;
        self.tree
            .borrow()
            .get(dir)
            .is_some_and(|n| n.is_directory())
            .then_some(dir)
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
