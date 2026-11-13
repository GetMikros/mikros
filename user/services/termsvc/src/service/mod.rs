//! The terminal service: pseudo-terminals whose master side is a stream
//! and whose slave side is served through `mikros.terminal.Terminal`,
//! each request naming its terminal by a token that the service made.
//! `Factory.Open` makes a terminal and returns its master stream, a
//! `Master` connection for its window size, and its token.

mod jobs;
mod notify;
mod serve;
mod tty;

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use core::cell::RefCell;
use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::Poll;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{HandleRole, LauncherAsyncClient};
use mikros_rt::interfaces::mikros::service::{ProviderAsyncServer, ProviderRequest};
use mikros_rt::interfaces::mikros::terminal::{
    Factory, FactoryAsyncServer, FactoryOpenResponse, FactoryRequest, Master, MasterAsyncServer,
    MasterRequest, MasterSetWindowSizeResponse, WindowSize,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use tty::Tty;

mikros_rt::raw_main!(main);

const FACTORY: &str = "mikros.terminal.Factory";
const TERMINAL: &str = "mikros.terminal.Terminal";
/// `object_get_info`'s basic topic, and the rights of a token.
const INFO_BASIC: u32 = 1;
const TOKEN_RIGHTS: u32 = 1 << 0 | 1 << 1 | 1 << 9;

/// What every connection shares: the terminals by the IDs of their
/// tokens' events, which the service keeps open so that the IDs stay
/// unique.
pub struct Service {
    pub reactor: Reactor,
    pub spawner: Spawner,
    pub launcher: LauncherAsyncClient,
    ttys: RefCell<BTreeMap<u64, (Rc<Tty>, OwnedHandle)>>,
}

impl Service {
    /// The terminal of `token`.
    pub fn tty(&self, token: &OwnedHandle) -> Option<Rc<Tty>> {
        let id = koid(token)?;
        self.ttys.borrow().get(&id).map(|(t, _)| t.clone())
    }

    /// The smallest number that no open terminal has: one whose master
    /// stream has not closed.
    fn free_number(&self) -> u32 {
        let ttys = self.ttys.borrow();
        let used: alloc::collections::BTreeSet<u32> = ttys
            .values()
            .filter(|(t, _)| !t.hung_up())
            .map(|(t, _)| t.number)
            .collect();
        (0..).find(|n| !used.contains(n)).unwrap_or(0)
    }

    /// Whether session `session` has a controlling terminal.
    pub fn has_controlling(&self, session: u32) -> bool {
        self.ttys
            .borrow()
            .values()
            .any(|(t, _)| t.session.get() == Some(session))
    }
}

/// `w` as rows, columns, width and height.
pub fn size(w: &WindowSize) -> [u16; 4] {
    [w.rows, w.columns, w.width, w.height]
}

pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// The object ID of `handle`'s object.
pub fn koid(handle: &OwnedHandle) -> Option<u64> {
    let mut info = [0u8; 24];
    let mut len = 0;
    // SAFETY: the pointers describe `info` and `len`.
    let status = unsafe {
        sys::object_get_info(
            handle.raw(),
            INFO_BASIC,
            info.as_mut_ptr(),
            info.len() as u64,
            &mut len,
        )
    };
    (status == mikros_rt::OK).then(|| u64::from_le_bytes(info[..8].try_into().unwrap()))
}

/// The output of `a` if it completes before `b`.
pub async fn first<A: Future, B: Future>(a: A, b: B) -> Option<A::Output> {
    let mut a = pin!(a);
    let mut b = pin!(b);
    poll_fn(|cx| {
        if let Poll::Ready(out) = a.as_mut().poll(cx) {
            return Poll::Ready(Some(out));
        }
        if b.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    // The service has no log handle: its messages go to the log service.
    mikros_rt::log::connect(&managed.header.directory);
    let Some(at) = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::Provider)
    else {
        return PANIC_EXIT_CODE;
    };
    let provider = managed.handles.remove(at).handle;
    let service = Rc::new(Service {
        reactor: reactor.clone(),
        spawner: spawner.clone(),
        launcher: LauncherAsyncClient::new(managed.header.launcher, reactor.clone()),
        ttys: RefCell::new(BTreeMap::new()),
    });
    let provider = ProviderAsyncServer::new(ServerEnd::new(provider), reactor.clone());
    executor.block_on(async {
        while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
            // Other protocols and named instances are not ours; dropping
            // the server end closes the connection.
            match (c.instance.is_none(), c.protocol.as_str()) {
                (true, FACTORY) => {
                    spawner.spawn(serve_factory(service.clone(), ServerEnd::new(c.server)))
                }
                (true, TERMINAL) => {
                    let end = ServerEnd::new(c.server);
                    spawner.spawn(serve::serve(service.clone(), end, c.client));
                }
                _ => {}
            }
        }
    });
    PANIC_EXIT_CODE
}

async fn serve_factory(service: Rc<Service>, end: ServerEnd<Factory>) {
    let server = FactoryAsyncServer::new(end, service.reactor.clone());
    while let Ok(FactoryRequest::Open(q, r)) = server.next().await {
        let opened = open(&service, size(&q.size));
        let _ = r.reply(server.server(), opened);
    }
}

/// Makes a terminal of window size `size`.
fn open(service: &Rc<Service>, size: [u16; 4]) -> Result<FactoryOpenResponse, Status> {
    let (mut ours, mut theirs) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    if unsafe { sys::stream_create(0, &mut ours, &mut theirs) } != mikros_rt::OK {
        return Err(Status::NoResources);
    }
    // SAFETY: the kernel just created the handles for us.
    let (ours, theirs) = unsafe { (OwnedHandle::from_raw(ours), OwnedHandle::from_raw(theirs)) };
    let mut event = 0;
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::event_create(0, &mut event) } != mikros_rt::OK {
        return Err(Status::NoResources);
    }
    // SAFETY: the kernel just created the handle for us.
    let event = unsafe { OwnedHandle::from_raw(event) };
    let mut token = 0;
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::handle_duplicate(event.raw(), TOKEN_RIGHTS, &mut token) } != mikros_rt::OK {
        return Err(Status::NoResources);
    }
    // SAFETY: the kernel just created the handle for us.
    let token = unsafe { OwnedHandle::from_raw(token) };
    let id = koid(&event).ok_or(Status::NoResources)?;
    let (control, control_server) = endpoints::<Master>().map_err(|_| Status::NoResources)?;
    let tty = Tty::new(service.free_number(), ours, size);
    service.ttys.borrow_mut().insert(id, (tty.clone(), event));
    let reactor = service.reactor.clone();
    service
        .spawner
        .spawn(tty.clone().read_master(reactor.clone()));
    service
        .spawner
        .spawn(tty.clone().write_master(reactor.clone()));
    service
        .spawner
        .spawn(jobs::events(service.clone(), tty.clone()));
    service
        .spawner
        .spawn(serve_master(tty, control_server, reactor));
    Ok(FactoryOpenResponse {
        master: theirs,
        master_control: control,
        terminal: token,
    })
}

async fn serve_master(tty: Rc<Tty>, end: ServerEnd<Master>, reactor: Reactor) {
    let server = MasterAsyncServer::new(end, reactor);
    while let Ok(MasterRequest::SetWindowSize(q, r)) = server.next().await {
        tty.set_size(size(&q.size));
        let _ = r.reply(server.server(), Ok(MasterSetWindowSizeResponse {}));
    }
}
