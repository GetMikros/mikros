//! Revocable views: every connection reached through one stays in its
//! domain, and revoking the domain closes them all and revokes the shared
//! memory objects handed out through them.

use crate::view::flat;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::{Future, poll_fn};
use core::pin::{Pin, pin};
use core::task::{Poll, Waker};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::fs::*;

/// A domain: whether it is revoked, whether its revocation failed, the
/// tasks that wait for it, and the controls of its shared memory objects.
#[derive(Default)]
pub struct Domain {
    revoked: Cell<bool>,
    failed: Cell<bool>,
    waiting: RefCell<Vec<Waker>>,
    controls: RefCell<Vec<MemoryRevocationAsyncClient>>,
}

impl Domain {
    pub fn is_revoked(&self) -> bool {
        self.revoked.get()
    }

    /// Resolves once the domain is revoked.
    async fn revoked(&self) {
        poll_fn(|cx| match self.revoked.get() {
            true => Poll::Ready(()),
            false => {
                // A connection polls this on every event, so a waker
                // that is there stays alone.
                let mut waiting = self.waiting.borrow_mut();
                if !waiting.iter().any(|w| w.will_wake(cx.waker())) {
                    waiting.push(cx.waker().clone());
                }
                Poll::Pending
            }
        })
        .await
    }

    /// Revokes the domain: its connections close, it takes no further
    /// requests, and each of its shared memory objects is revoked, which
    /// it waits for. Fails with `IO` when one could not be, and the domain
    /// stays closed.
    pub async fn revoke(&self) -> Result<(), Error> {
        self.revoked.set(true);
        for waker in self.waiting.borrow_mut().drain(..) {
            waker.wake();
        }
        let controls = core::mem::take(&mut *self.controls.borrow_mut());
        for control in controls {
            if !matches!(control.revoke().await, Ok(Ok(_))) {
                self.failed.set(true);
            }
        }
        match self.failed.get() {
            true => Err(Error::Io),
            false => Ok(()),
        }
    }
}

/// The domains of the server, by key.
#[derive(Default)]
pub struct Domains {
    map: RefCell<BTreeMap<u64, Rc<Domain>>>,
}

impl Domains {
    /// The domain of `key`: a new one when it has none, or when its domain
    /// was revoked without failing.
    pub fn get(&self, key: u64) -> Result<Rc<Domain>, Error> {
        let mut map = self.map.borrow_mut();
        if let Some(domain) = map.get(&key) {
            if domain.failed.get() {
                return Err(Error::Io);
            }
            if !domain.is_revoked() {
                return Ok(domain.clone());
            }
        }
        let domain = Rc::new(Domain::default());
        map.insert(key, domain.clone());
        Ok(domain)
    }

    /// Revokes the domain of `key`; `NOT_FOUND` without one.
    pub async fn revoke(&self, key: u64) -> Result<(), Error> {
        let domain = self
            .map
            .borrow()
            .get(&key)
            .cloned()
            .ok_or(Error::NotFound)?;
        domain.revoke().await
    }
}

/// A connection of a revocable view: its domain, the backing node, and
/// its rights.
struct Connection {
    domain: Rc<Domain>,
    backing: NodeAsyncClient,
    rights: Rights,
    reactor: Reactor,
    spawner: Spawner,
}

/// Whether `request` is one of the calls that a view's connection serves
/// while another call of it waits for its backing connection
/// (MIKSPEC-0051.R18).
fn never_waits(request: &NodeRequest) -> bool {
    matches!(
        request,
        NodeRequest::GetAttributes(..)
            | NodeRequest::Access(..)
            | NodeRequest::QueryFilesystem(..)
            | NodeRequest::Clone(..)
    )
}

type Handling<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

/// What a connection's loop learned.
enum Event {
    Request(Result<NodeRequest, mikros_rt::interfaces::__midl::Error>),
    Finished,
    Revoked,
}

/// Serves the connection on `end` to `backing` with `rights` in `domain`,
/// forwarding each request to `backing`, until the client closes it or
/// the domain is revoked, which closes both. The calls go one after the
/// other in the order they arrive, except that, while one waits for
/// `backing`, a call that never waits is served as it arrives
/// (MIKSPEC-0051.R18).
pub async fn serve(
    domain: Rc<Domain>,
    backing: NodeAsyncClient,
    end: ServerEnd<Node>,
    rights: Rights,
    reactor: Reactor,
    spawner: Spawner,
) {
    if domain.is_revoked() {
        return;
    }
    let server = NodeAsyncServer::new(end, reactor.clone());
    let connection = Connection {
        domain,
        backing,
        rights,
        reactor,
        spawner,
    };
    let mut revoked = pin!(connection.domain.revoked());
    let mut next: Pin<Box<dyn Future<Output = _> + '_>> = Box::pin(server.next());
    let mut current: Option<Handling<'_>> = None;
    let mut queued: VecDeque<NodeRequest> = VecDeque::new();
    let mut meanwhile: Vec<Handling<'_>> = Vec::new();
    loop {
        if current.is_none()
            && let Some(request) = queued.pop_front()
        {
            current = Some(Box::pin(connection.handle(request, server.server())));
        }
        let event = poll_fn(|cx| {
            if revoked.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Event::Revoked);
            }
            meanwhile.retain_mut(|call| call.as_mut().poll(cx).is_pending());
            if let Some(call) = current.as_mut()
                && call.as_mut().poll(cx).is_ready()
            {
                return Poll::Ready(Event::Finished);
            }
            match next.as_mut().poll(cx) {
                Poll::Ready(request) => Poll::Ready(Event::Request(request)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;
        match event {
            Event::Revoked => break,
            Event::Finished => current = None,
            Event::Request(Err(_)) => break,
            Event::Request(Ok(request)) => {
                next = Box::pin(server.next());
                if connection.domain.is_revoked() {
                    break;
                }
                let busy = current.is_some() || !queued.is_empty();
                match busy && never_waits(&request) {
                    true => meanwhile.push(Box::pin(connection.handle(request, server.server()))),
                    false => queued.push_back(request),
                }
            }
        }
    }
}

impl Connection {
    fn require(&self, rights: Rights) -> Result<(), Error> {
        match self.rights.contains(rights) {
            true => Ok(()),
            false => Err(Error::AccessDenied),
        }
    }

    /// Fails with `ACCESS_DENIED` once the domain is revoked, for what
    /// would grant new access.
    fn open_domain(&self) -> Result<(), Error> {
        match self.domain.is_revoked() {
            true => Err(Error::AccessDenied),
            false => Ok(()),
        }
    }

    /// Serves a new connection of the domain on `end` to `backing`.
    fn spawn(&self, backing: NodeAsyncClient, end: ServerEnd<Node>, rights: Rights) {
        let task = serve(
            self.domain.clone(),
            backing,
            end,
            rights,
            self.reactor.clone(),
            self.spawner.clone(),
        );
        self.spawner.spawn(task);
    }

    async fn handle(&self, request: NodeRequest, s: &NodeServer) {
        use NodeRequest as R;
        let b = &self.backing;
        // A reply that cannot be sent means that the client went away,
        // which the next read finds.
        let _ = match request {
            R::GetAttributes(_, r) => r.reply(s, flat(b.get_attributes().await)),
            R::SetAttributes(q, r) => r.reply(
                s,
                flat(
                    b.set_attributes(q.mask, q.mode, q.accessed, q.modified)
                        .await,
                ),
            ),
            R::Clone(q, r) => r.reply(s, self.clone(q).await),
            R::Sync(_, r) => r.reply(s, flat(b.sync().await)),
            R::QueryFilesystem(_, r) => r.reply(s, flat(b.query_filesystem().await)),
            R::Read(q, r) => r.reply(s, flat(b.read(q.offset, q.count).await)),
            R::Write(q, r) => r.reply(s, flat(b.write(q.offset, q.data).await)),
            R::Append(q, r) => r.reply(s, flat(b.append(q.data).await)),
            R::Truncate(q, r) => r.reply(s, flat(b.truncate(q.size).await)),
            R::GetMemoryObject(q, r) => r.reply(s, self.memory(q.flags).await),
            R::GetRevocableMemoryObject(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::SetOwner(q, r) => r.reply(s, flat(b.set_owner(q.owner).await)),
            R::GetAcl(q, r) => r.reply(s, flat(b.get_acl(q.default).await)),
            R::Access(_, r) => r.reply(s, flat(b.access().await)),
            R::SetAcl(q, r) => r.reply(s, flat(b.set_acl(q.default, q.entries).await)),
            R::Restrict(q, r) => r.reply(s, self.restrict(q).await),
            R::Exchange(q, r) => r.reply(s, flat(b.exchange(q.name, q.target, q.other_name).await)),
            R::CloneRange(q, r) => r.reply(
                s,
                flat(
                    b.clone_range(q.source, q.source_offset, q.offset, q.length)
                        .await,
                ),
            ),
            R::Lock(q, r) => r.reply(s, flat(b.lock(q.kind, q.start, q.length, q.wait).await)),
            R::Unlock(q, r) => r.reply(s, flat(b.unlock(q.start, q.length).await)),
            R::ReadLink(_, r) => r.reply(s, flat(b.read_link().await)),
            R::Seek(q, r) => r.reply(s, flat(b.seek(q.offset, q.origin).await)),
            R::ReadNext(q, r) => r.reply(s, flat(b.read_next(q.count).await)),
            R::WriteNext(q, r) => r.reply(s, flat(b.write_next(q.data).await)),
            R::Open(q, r) => r.reply(s, self.open(q).await),
            R::ReadDirectory(q, r) => {
                r.reply(s, flat(b.read_directory(q.cookie, q.attributes).await))
            }
            R::Unlink(q, r) => r.reply(s, flat(b.unlink(q.name, q.directory).await)),
            R::GetToken(_, r) => r.reply(s, flat(b.get_token().await)),
            R::Rename(q, r) => r.reply(s, flat(b.rename(q.name, q.target, q.new_name).await)),
            R::Link(q, r) => r.reply(s, flat(b.link(q.name, q.target, q.new_name).await)),
            R::CreateSymlink(q, r) => r.reply(s, flat(b.create_symlink(q.name, q.target).await)),
            R::Watch(q, r) => r.reply(s, flat(b.watch(q.mask, q.cookie, q.watcher).await)),
            R::Unwatch(q, r) => r.reply(s, flat(b.unwatch(q.cookie).await)),
            R::BindSocket(q, r) => {
                r.reply(s, flat(b.bind_socket(q.name, q.mode, q.listener).await))
            }
            R::ConnectSocket(q, r) => r.reply(
                s,
                flat(b.connect_socket(q.name, q.r#type, q.connection).await),
            ),
        };
    }

    /// Restricts the backing connection, and serves the restricted one as
    /// a connection of the domain with the same rights.
    async fn restrict(&self, q: NodeRestrictRequest) -> Result<NodeRestrictResponse, Error> {
        let (backing, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
        flat(
            self.backing
                .restrict(
                    q.user,
                    q.app,
                    q.pid,
                    q.koid,
                    q.admin,
                    q.portable_checksum,
                    server,
                )
                .await,
        )?;
        self.open_domain()?;
        self.spawn(
            NodeAsyncClient::new(backing, self.reactor.clone()),
            q.node,
            self.rights,
        );
        Ok(NodeRestrictResponse {})
    }

    async fn clone(&self, q: NodeCloneRequest) -> Result<NodeCloneResponse, Error> {
        self.require(q.rights)?;
        let (backing, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
        flat(self.backing.clone(server, q.rights).await)?;
        self.open_domain()?;
        self.spawn(
            NodeAsyncClient::new(backing, self.reactor.clone()),
            q.node,
            q.rights,
        );
        Ok(NodeCloneResponse {})
    }

    async fn open(&self, q: NodeOpenRequest) -> Result<NodeOpenResponse, Error> {
        self.require(q.rights)?;
        let (backing, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
        let opened = flat(
            self.backing
                .open(q.path, q.flags, q.mode, q.rights, server)
                .await,
        )?;
        self.open_domain()?;
        if opened.redirect.is_none() {
            self.spawn(
                NodeAsyncClient::new(backing, self.reactor.clone()),
                q.node,
                q.rights,
            );
        }
        Ok(opened)
    }

    /// A memory object of the file: with `PRIVATE` the backing node's own
    /// copy, which revocation leaves alone, and otherwise a revocable one
    /// from the backing node, whose control the domain keeps; `NOT_SUPPORTED`
    /// when the backing node has none.
    async fn memory(&self, flags: MemoryFlags) -> Result<NodeGetMemoryObjectResponse, Error> {
        if flags.contains(MemoryFlags::PRIVATE) {
            return flat(self.backing.get_memory_object(flags).await);
        }
        let got = flat(self.backing.get_revocable_memory_object(flags).await)?;
        let control = MemoryRevocationAsyncClient::new(got.control, self.reactor.clone());
        if self.domain.is_revoked() {
            let _ = control.revoke().await;
            return Err(Error::AccessDenied);
        }
        self.domain.controls.borrow_mut().push(control);
        Ok(NodeGetMemoryObjectResponse { memory: got.memory })
    }
}
