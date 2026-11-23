//! The gate of a pending entry (MIKSPEC-0094.R2, MIKSPEC-0094.R8): a
//! directory that the process manager serves over `Node`, which the
//! union server shows as a revocable view and binds to each client
//! connection's subject with `Restrict`. The first call that needs an
//! answer asks the broker with `Request` for the process, and its
//! calls, and every later one, go on to the folder once it is
//! `ALLOWED`. `crate::gate` decides who asks and who waits.

mod forward;

use crate::gate::{Answer, Arrival, Machine, Outcome, Target};
use crate::program::processes::Shared;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};
use forward::{forward, refuse};
use mikros_acl::{Checksum, Subject};
use mikros_rt::interfaces::__midl::{ClientEnd, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::*;
use mikros_rt::interfaces::mikros::permissions::Decision;
use mikros_rt::interfaces::mikros::service::ClientIdentity;

const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);
/// The mode that a directory shows before it has an answer.
const MODE: u32 = 0o555;

/// A waiting call's place for what becomes of it.
#[derive(Default)]
struct Slot {
    outcome: Cell<Option<Outcome<Error>>>,
    waker: Cell<Option<Waker>>,
}

impl Slot {
    fn resolve(&self, outcome: Outcome<Error>) {
        self.outcome.set(Some(outcome));
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    async fn outcome(&self) -> Outcome<Error> {
        poll_fn(|cx| match self.outcome.take() {
            Some(outcome) => Poll::Ready(outcome),
            None => {
                self.waker.set(Some(cx.waker().clone()));
                Poll::Pending
            }
        })
        .await
    }
}

/// One subject of a gate: its state, and the connection to the folder
/// that is bound to it once `ALLOWED`.
struct SubjectState {
    subject: Subject,
    machine: RefCell<Machine<Rc<Slot>>>,
    master: RefCell<Option<Rc<NodeAsyncClient>>>,
}

/// A pending entry: for one user, the app of its view's key, and one
/// permission.
pub struct Gate {
    shared: Rc<Shared>,
    user: u32,
    app_id: String,
    permission: &'static str,
    target: Target,
    /// The folder's path, for a folder.
    path: String,
    revoked: Cell<bool>,
    subjects: RefCell<Vec<Rc<SubjectState>>>,
}

impl Gate {
    /// The state of `subject`, made when it has none.
    fn subject(&self, subject: Subject) -> Rc<SubjectState> {
        let mut subjects = self.subjects.borrow_mut();
        if let Some(state) = subjects.iter().find(|s| s.subject == subject) {
            return state.clone();
        }
        let state = Rc::new(SubjectState {
            subject,
            machine: RefCell::new(Machine::new(self.revoked.get())),
            master: RefCell::new(None),
        });
        subjects.push(state.clone());
        state
    }

    /// Revokes the gate: every call that waits fails, and nothing is
    /// asked again for any subject.
    pub fn revoke(&self) {
        self.revoked.set(true);
        let subjects = self.subjects.borrow().clone();
        for state in subjects {
            let outcomes = state.machine.borrow_mut().revoke();
            for (slot, outcome) in outcomes {
                slot.resolve(outcome);
            }
            *state.master.borrow_mut() = None;
        }
    }

    /// What the broker says about the process of `subject`, and when
    /// it is `ALLOWED`, the folder bound to `subject`.
    async fn question(&self, state: &SubjectState) -> Answer<Error> {
        let subject = state.subject;
        let client = {
            let records = self.shared.records.borrow();
            match records.get(subject.pid) {
                Some(r) if r.koid == subject.koid && r.user == subject.user => ClientIdentity {
                    pid: subject.pid,
                    user: r.user,
                    app_id: r.app_id.clone(),
                    admin: r.admin,
                    koid: r.koid,
                    portable_checksum: r.checksum(),
                },
                _ => return Answer::Failed(Error::NotFound),
            }
        };
        mikros_rt::log!(
            Info,
            "process-manager: asking {} for process {} ({})",
            self.permission,
            subject.pid,
            client.app_id
        );
        let decision = self
            .shared
            .permissions
            .request(&self.shared.reactor, client, self.permission)
            .await;
        match decision {
            Ok(Decision::Allowed) => {}
            Ok(_) => return Answer::Denied,
            Err(status) => return Answer::Failed(error(status)),
        }
        match self.bind(subject).await {
            Ok(master) => {
                *state.master.borrow_mut() = Some(Rc::new(master));
                Answer::Allowed
            }
            Err(e) => Answer::Failed(e),
        }
    }

    /// The folder, opened as MIKSPEC-0050.R38 and MIKSPEC-0064.R21 give
    /// it, with `subject` bound to the connection.
    async fn bind(&self, subject: Subject) -> Result<NodeAsyncClient, Error> {
        let directory = match self.target {
            Target::Volumes => self.shared.volumes.directory().map_err(error)?,
            Target::Folder(_) => {
                let folder = self
                    .shared
                    .namespaces
                    .folder(&self.shared, &self.path, RW)
                    .await
                    .map_err(error)?;
                folder.0.directory
            }
        };
        let directory = NodeAsyncClient::new(directory, self.shared.reactor.clone());
        let (client, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
        let Subject {
            user,
            app,
            pid,
            koid,
            admin,
            portable_checksum,
        } = subject;
        let checksum = portable_checksum.as_str().into();
        flat(
            directory
                .restrict(user, app, pid, koid, admin, checksum, server)
                .await,
        )?;
        Ok(NodeAsyncClient::new(client, self.shared.reactor.clone()))
    }
}

fn flat<T>(
    reply: Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>,
) -> Result<T, Error> {
    reply.unwrap_or(Err(Error::Io))
}

/// The error of the file system protocol for a failed call to the
/// broker or to a service.
fn error(status: Status) -> Error {
    match status {
        Status::NotFound => Error::NotFound,
        Status::AccessDenied => Error::AccessDenied,
        Status::InvalidArgs => Error::Invalid,
        Status::NotSupported => Error::NotSupported,
        Status::BadState => Error::BadState,
        _ => Error::Io,
    }
}

/// What a call that needs an answer becomes: it asks, or waits for the
/// question that is open, or is told at once.
async fn answer(gate: &Rc<Gate>, state: &Rc<SubjectState>) -> Outcome<Error> {
    let slot = Rc::new(Slot::default());
    let arrival = state.machine.borrow_mut().arrive(slot.clone());
    match arrival {
        Arrival::Ask => {
            let (gate, state) = (gate.clone(), state.clone());
            let reactor = gate.shared.spawner.clone();
            reactor.spawn(async move { ask(gate, state).await });
            slot.outcome().await
        }
        Arrival::Wait => slot.outcome().await,
        Arrival::Proceed => Outcome::Proceed,
        Arrival::Refuse => Outcome::Denied,
    }
}

/// Makes the `Request` of `state`, and tells the calls that waited for it.
/// It is a task of its own, so that it ends however the connections do.
async fn ask(gate: Rc<Gate>, state: Rc<SubjectState>) {
    let result = gate.question(&state).await;
    let outcomes = state.machine.borrow_mut().ended(result);
    if !state.machine.borrow().allowed() {
        *state.master.borrow_mut() = None;
    }
    for (slot, outcome) in outcomes {
        slot.resolve(outcome);
    }
}

/// A pending view for the grant of `permission` to `app_id` for `user`,
/// of the folder at `path` or the volumes, with `rights`. None when the
/// view cannot be made, or was revoked meanwhile.
pub async fn view(
    shared: &Rc<Shared>,
    (user, app_id, permission): (u32, &str, &'static str),
    (target, path): (Target, String),
    rights: Rights,
) -> Option<ClientEnd<Node>> {
    let gate = Rc::new(Gate {
        shared: shared.clone(),
        user,
        app_id: app_id.into(),
        permission,
        target,
        path,
        revoked: Cell::new(false),
        subjects: RefCell::new(Vec::new()),
    });
    shared
        .grants
        .add_gate(user, app_id, permission, Rc::downgrade(&gate));
    let (client, server) = endpoints::<Node>().ok()?;
    shared
        .spawner
        .spawn(serve(gate.clone(), server, rights, None));
    crate::program::grants::view(shared, (user, app_id, permission), client, rights).await
}

/// One connection to the gate, with `rights`, and the subject it is bound
/// to, if any.
struct Connection {
    gate: Rc<Gate>,
    rights: Rights,
    state: Option<Rc<SubjectState>>,
    /// The connection to the folder that this connection forwards to.
    bound: RefCell<Option<Rc<NodeAsyncClient>>>,
}

async fn serve(
    gate: Rc<Gate>,
    end: ServerEnd<Node>,
    rights: Rights,
    state: Option<Rc<SubjectState>>,
) {
    let server = Rc::new(NodeAsyncServer::new(end, gate.shared.reactor.clone()));
    let connection = Rc::new(Connection {
        gate,
        rights,
        state,
        bound: RefCell::new(None),
    });
    while let Ok(request) = server.next().await {
        connection.handle(request, &server).await;
    }
}

/// What a directory without an answer shows.
fn attributes() -> Attributes {
    Attributes {
        r#type: NodeType::Directory,
        mode: MODE,
        owner: 0,
        size: 0,
        allocated: 0,
        links: 2,
        inode: 1,
        created: 0,
        modified: 0,
        accessed: 0,
        changed: 0,
    }
}

impl Connection {
    /// Serves a new connection of the gate on `end`, with the subject of
    /// this one.
    fn spawn(&self, end: ServerEnd<Node>, rights: Rights) {
        let task = serve(self.gate.clone(), end, rights, self.state.clone());
        self.gate.shared.spawner.spawn(task);
    }

    async fn handle(self: &Rc<Self>, request: NodeRequest, server: &Rc<NodeAsyncServer>) {
        use NodeRequest as R;
        let s = server.server();
        let allowed = self
            .state
            .as_ref()
            .is_some_and(|state| state.machine.borrow().allowed());
        // The calls that never wait, and `Restrict`.
        let request = match request {
            R::GetAttributes(_, r) if !allowed => {
                let _ = r.reply(
                    s,
                    Ok(NodeGetAttributesResponse {
                        attributes: attributes(),
                    }),
                );
                return;
            }
            R::Access(_, r) if !allowed => {
                let _ = r.reply(
                    s,
                    Ok(NodeAccessResponse {
                        rights: Rights::READ,
                    }),
                );
                return;
            }
            R::QueryFilesystem(_, r) if !allowed => {
                let _ = r.reply(s, Err(Error::NotSupported));
                return;
            }
            R::Clone(q, r) if !allowed => {
                let cloned = match self.rights.contains(q.rights) {
                    true => {
                        self.spawn(q.node, q.rights);
                        Ok(NodeCloneResponse {})
                    }
                    false => Err(Error::AccessDenied),
                };
                let _ = r.reply(s, cloned);
                return;
            }
            R::Restrict(q, r) => {
                let _ = r.reply(s, self.restrict(q));
                return;
            }
            other => other,
        };
        let Some(state) = self.state.clone() else {
            refuse(request, s, Error::AccessDenied);
            return;
        };
        if allowed {
            self.go_on(&state, request, s).await;
            return;
        }
        let (connection, server) = (self.clone(), server.clone());
        self.gate.shared.spawner.spawn(async move {
            let s = server.server();
            match answer(&connection.gate, &state).await {
                Outcome::Proceed => connection.go_on(&state, request, s).await,
                Outcome::Denied => refuse(request, s, Error::AccessDenied),
                Outcome::Failed(e) => refuse(request, s, e),
            }
        });
    }

    /// Binds a new connection of the gate to the subject in `q`, unless
    /// this one has a subject already.
    fn restrict(&self, q: NodeRestrictRequest) -> Result<NodeRestrictResponse, Error> {
        if self.state.is_some() {
            return Err(Error::BadState);
        }
        let portable_checksum = Checksum::new(&q.portable_checksum).ok_or(Error::Invalid)?;
        let subject = Subject {
            user: q.user,
            app: q.app,
            pid: q.pid,
            koid: q.koid,
            admin: q.admin,
            portable_checksum,
        };
        let state = self.gate.subject(subject);
        let task = serve(self.gate.clone(), q.node, self.rights, Some(state));
        self.gate.shared.spawner.spawn(task);
        Ok(NodeRestrictResponse {})
    }

    /// Forwards `request` to this connection's own connection to the
    /// folder, made from the one bound to the subject.
    async fn go_on(&self, state: &SubjectState, request: NodeRequest, s: &NodeServer) {
        match self.folder(state).await {
            Ok(folder) => forward(&folder, request, s).await,
            Err(e) => refuse(request, s, e),
        }
    }

    async fn folder(&self, state: &SubjectState) -> Result<Rc<NodeAsyncClient>, Error> {
        if let Some(folder) = self.bound.borrow().clone() {
            return Ok(folder);
        }
        let master = state.master.borrow().clone().ok_or(Error::AccessDenied)?;
        let (client, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
        flat((*master).clone(server, self.rights).await)?;
        let folder = Rc::new(NodeAsyncClient::new(
            client,
            self.gate.shared.reactor.clone(),
        ));
        let mut bound = self.bound.borrow_mut();
        Ok(bound.get_or_insert(folder).clone())
    }
}
