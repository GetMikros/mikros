//! The file system's tree, the connections open to its nodes, the
//! tokens and budgets of directories, and the `Filesystem` service.

use crate::memfs::{Id, ROOT, Tree};
use crate::service::file::File;
use crate::service::node;
use crate::service::watch::{Watches, event};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicU64, Ordering, fence};
use core::task::Waker;
use mikros_acl::Subject;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemAsyncServer, FilesystemGetRootResponse, FilesystemInfo,
    FilesystemQueryFilesystemResponse, FilesystemRequest, Node, Rights, SocketListenerClient,
    WatchKind,
};
use mikros_rt::interfaces::mikros::permissions::{
    AppNumbers, AppNumbersAsyncClient, Broker, BrokerAsyncClient,
};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::time::{Clock, ClockAsyncClient};
use mikros_rt::sys;

/// The broker's protocols, through which a subject's `admin.acls.manage`
/// is asked for.
const BROKER: &str = "mikros.permissions.Broker";
const CLOCK: &str = "mikros.time.Clock";
/// The address of the time service's time page once it is mapped, 0
/// before.
static TIME_PAGE: AtomicU64 = AtomicU64::new(0);
const APP_NUMBERS: &str = "mikros.permissions.AppNumbers";

const INFO_BASIC: u32 = 1;
/// The rights of a token: DUPLICATE, TRANSFER and INSPECT.
const TOKEN_RIGHTS: u32 = 1 << 0 | 1 << 1 | 1 << 9;
const PAGE: u32 = 4096;

pub type Nodes = Tree<File, Socket>;

/// What a socket node refers to: the listener that its connections go
/// to.
pub struct Socket {
    pub listener: SocketListenerClient,
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

/// The locks of a file, and the tasks that wait for one to go.
#[derive(Default)]
struct Locks {
    held: Vec<Lock>,
    waiting: Vec<Waker>,
}

pub struct Fs {
    pub tree: RefCell<Nodes>,
    /// How many connections each node has.
    refs: RefCell<BTreeMap<Id, usize>>,
    /// The directories that tokens name, by the token's object ID.
    tokens: RefCell<BTreeMap<u64, Id>>,
    /// The byte-range locks of files, and the next owner of locks.
    locks: RefCell<BTreeMap<Id, Locks>>,
    next_owner: Cell<u64>,
    /// The budgets that directories charge the memory of the files below
    /// them to.
    pub budgets: RefCell<BTreeMap<Id, Rc<OwnedHandle>>>,
    pub watches: Watches,
    /// The service's own directory, through which it reaches the broker.
    pub(crate) directory: DirectoryAsyncClient,
    pub reactor: Reactor,
    pub spawner: Spawner,
}

/// The protocol's error for the tree's.
pub fn error(e: crate::memfs::Error) -> Error {
    use crate::memfs::Error as E;
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
    }
}

/// The time now, in nanoseconds since 1970 in UTC, from the time page
/// (MIKSPEC-0054.R9, MIKSPEC-0050.R54), or 0, an unknown time, while
/// tmpfs, which starts before the time service, has none.
pub fn now() -> i64 {
    let page = TIME_PAGE.load(Ordering::Acquire);
    if page == 0 {
        return 0;
    }
    let read = |at: u64| {
        // SAFETY: the page stays mapped read-only for as long as the
        // process runs.
        unsafe { core::ptr::read_volatile(at as *const u64) }
    };
    // The page's first word is odd while the time service changes it.
    let offset = loop {
        let before = read(page) as u32;
        fence(Ordering::Acquire);
        let offset = read(page + 8) as i64;
        fence(Ordering::Acquire);
        if before % 2 == 0 && read(page) as u32 == before {
            break offset;
        }
        core::hint::spin_loop();
    };
    let mut monotonic = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut monotonic) };
    monotonic.saturating_add(offset)
}

/// Waits for the time service, through `directory`, and maps its time
/// page into `process`, from which `now` then gives UTC.
pub async fn take_time_page(
    directory: &DirectoryAsyncClient,
    reactor: Reactor,
    process: sys::Handle,
) {
    const READ: u32 = 1 << 0;
    let Ok((client, server)) = endpoints::<Clock>() else {
        return;
    };
    // The connection waits until the time service runs.
    let connected = directory
        .connect(CLOCK.into(), None, server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        mikros_rt::log!(
            Warning,
            "tmpfs: the time service cannot be reached; times stay 0"
        );
        return;
    }
    let Ok(reply) = ClockAsyncClient::new(client, reactor).get_time_page().await else {
        return;
    };
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let status = unsafe {
        sys::vm_map(
            process,
            READ,
            0,
            reply.page.raw(),
            0,
            u64::from(PAGE),
            &mut at,
        )
    };
    if status == mikros_rt::OK {
        TIME_PAGE.store(at, Ordering::Release);
    }
}

impl Fs {
    pub fn new(directory: DirectoryAsyncClient, reactor: Reactor, spawner: Spawner) -> Fs {
        Fs {
            tree: RefCell::new(Tree::new(0o755, 0, now())),
            refs: RefCell::new(BTreeMap::new()),
            tokens: RefCell::new(BTreeMap::new()),
            locks: RefCell::new(BTreeMap::new()),
            next_owner: Cell::new(1),
            budgets: RefCell::new(BTreeMap::new()),
            watches: Watches::default(),
            directory,
            reactor,
            spawner,
        }
    }

    /// Whether the broker allows `subject` `admin.acls.manage`, through
    /// connections of their own; not when they cannot be made.
    pub async fn may_manage(&self, subject: &Subject) -> bool {
        let (Ok((broker, broker_server)), Ok((numbers, numbers_server))) =
            (endpoints::<Broker>(), endpoints::<AppNumbers>())
        else {
            return false;
        };
        for (protocol, server) in [
            (BROKER, broker_server.into_handle()),
            (APP_NUMBERS, numbers_server.into_handle()),
        ] {
            let connected = self.directory.connect(protocol.into(), None, server).await;
            if !matches!(connected, Ok(Ok(_))) {
                return false;
            }
        }
        let broker = BrokerAsyncClient::new(broker, self.reactor.clone());
        let numbers = AppNumbersAsyncClient::new(numbers, self.reactor.clone());
        mikros_acl::midl::may_manage(&broker, &numbers, subject).await
    }

    /// Serves `end` as a connection to `node` with `rights` for
    /// `subject`, if any, writing at the end when `append`, and looking
    /// names up case-insensitively when `folded` (MIKSPEC-0071.R17).
    pub fn serve(
        self: &Rc<Self>,
        end: ServerEnd<Node>,
        node: Id,
        rights: Rights,
        subject: Option<Subject>,
        append: bool,
        folded: bool,
    ) {
        *self.refs.borrow_mut().entry(node).or_default() += 1;
        let task = node::serve(self.clone(), end, node, rights, subject, append, folded);
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
                self.forget(node);
            }
        }
    }

    /// Lets go of `node` after its last name went, unless a connection
    /// still holds it.
    pub fn unnamed(&self, node: Id) {
        if !self.refs.borrow().contains_key(&node) {
            self.forget(node);
        }
    }

    /// Lets go of `node`, and of the budget it had, once nothing names
    /// it.
    fn forget(&self, node: Id) {
        if self.tree.borrow_mut().forget(node).is_some() {
            self.budgets.borrow_mut().remove(&node);
        }
    }

    /// Tells the watches of `node` and of the directories that name it
    /// that its contents or attributes changed, as `kind` says.
    pub fn changed(&self, node: Id, kind: WatchKind) {
        self.watches.notify(node, &[event(kind, 0, "")]);
        let names = self
            .tree
            .borrow()
            .get(node)
            .map(|n| n.names.clone())
            .unwrap_or_default();
        for (dir, name) in names {
            self.watches.notify(dir, &[event(kind, 0, &name)]);
        }
    }

    /// A new owner of locks, for a connection.
    pub fn owner(&self) -> u64 {
        let owner = self.next_owner.get();
        self.next_owner.set(owner + 1);
        owner
    }

    /// Takes the lock of `owner` on `start..end` of the file `id`,
    /// replacing its own locks there, unless another owner's lock
    /// conflicts: then false, and `waker`, when given, is woken once a
    /// lock goes.
    pub fn try_lock(
        &self,
        id: Id,
        owner: u64,
        exclusive: bool,
        (start, end): (u64, u64),
        waker: Option<&Waker>,
    ) -> bool {
        let mut locks = self.locks.borrow_mut();
        let file = locks.entry(id).or_default();
        let conflicts = file.held.iter().any(|l| {
            l.owner != owner && l.start < end && start < l.end && (exclusive || l.exclusive)
        });
        if conflicts {
            if let Some(waker) = waker {
                file.waiting.push(waker.clone());
            }
            return false;
        }
        cut(&mut file.held, owner, start, end);
        file.held.push(Lock {
            owner,
            exclusive,
            start,
            end,
        });
        true
    }

    /// Releases the locks of `owner` on `start..end` of the file `id`.
    pub fn unlock(&self, id: Id, owner: u64, (start, end): (u64, u64)) {
        let mut locks = self.locks.borrow_mut();
        let Some(file) = locks.get_mut(&id) else {
            return;
        };
        cut(&mut file.held, owner, start, end);
        for waker in file.waiting.drain(..) {
            waker.wake();
        }
        if file.held.is_empty() && file.waiting.is_empty() {
            locks.remove(&id);
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
            label: alloc::string::String::new(),
            id: alloc::string::String::new(),
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

/// Serves `Filesystem` on `end`: `GetRoot` serves a connection to the
/// root directory, without a subject.
pub async fn serve_filesystem(fs: Rc<Fs>, end: ServerEnd<Filesystem>) {
    let server = FilesystemAsyncServer::new(end, fs.reactor.clone());
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            FilesystemRequest::GetRoot(q, r) => {
                fs.serve(q.node, ROOT, q.rights, None, false, false);
                r.reply(s, Ok(FilesystemGetRootResponse {}))
            }
            FilesystemRequest::QueryFilesystem(_, r) => {
                r.reply(s, Ok(FilesystemQueryFilesystemResponse { info: fs.info() }))
            }
        };
    }
}
