//! What the service manager tells of each service of a valid manifest:
//! its state, its restarts since boot and the status of its last exit,
//! and `mikros.service.Services`, which lists them to every process.

use crate::directory::Dir;
use crate::supervisor::Supervisor;
use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_manifest::Start;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::service::{
    ServiceEntry, ServiceState, Services, ServicesAsyncServer, ServicesListResponse,
    ServicesRequest,
};

/// The most entries that one `List` gives.
const PAGE: usize = 64;

/// The state of a service.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum State {
    /// Not started since boot.
    #[default]
    NotStarted,
    Running,
    /// Ended, and to be started again as its manifest says.
    Restarting,
    /// Ended, and not restarted.
    Exited,
    /// Restarted too often, or not started.
    Failed,
}

/// What `List` tells of a service besides its manifest.
#[derive(Clone, Copy, Default)]
pub struct Status {
    pub state: State,
    pub restarts: u32,
    pub exit_status: i32,
}

impl State {
    fn wire(self) -> ServiceState {
        match self {
            State::NotStarted => ServiceState::NotStarted,
            State::Running => ServiceState::Running,
            State::Restarting => ServiceState::Restarting,
            State::Exited => ServiceState::Exited,
            State::Failed => ServiceState::Failed,
        }
    }
}

impl Supervisor {
    /// Sets the state of service `i`.
    pub fn set_state(&self, i: usize, state: State) {
        self.status.borrow_mut().entry(i).or_default().state = state;
    }

    /// Notes that service `i` ended with exit status `code`, and that it
    /// is restarted when `restarting`.
    pub fn ended(&self, i: usize, code: i64, restarting: bool) {
        let mut status = self.status.borrow_mut();
        let entry = status.entry(i).or_default();
        entry.exit_status = i32::try_from(code).unwrap_or(i32::MIN);
        if restarting {
            entry.restarts = entry.restarts.saturating_add(1);
            entry.state = State::Restarting;
        }
    }

    /// The services after the name `after`, or from the first, sorted by
    /// name, at most `PAGE` of them, and whether more follow.
    pub fn entries(&self, after: Option<&str>) -> (Vec<ServiceEntry>, bool) {
        let services = self.services.borrow();
        // A removed package's service has no manifest any more.
        let removed = self.removed.borrow();
        let mut order: Vec<usize> = (0..services.len())
            .filter(|i| !removed.contains(i))
            .filter(|&i| after.is_none_or(|after| services[i].name.as_str() > after))
            .collect();
        order.sort_by(|&a, &b| services[a].name.cmp(&services[b].name));
        let (status, running) = (self.status.borrow(), self.running.borrow());
        let more = order.len() > PAGE;
        let entries = order
            .into_iter()
            .take(PAGE)
            .map(|i| {
                let s = status.get(&i).copied().unwrap_or_default();
                let pid = running
                    .iter()
                    .find(|&&(j, _)| j == i)
                    .map_or(0, |&(_, p)| p);
                ServiceEntry {
                    name: services[i].name.clone(),
                    state: s.state.wire(),
                    on_demand: services[i].start == Start::OnDemand,
                    pid,
                    restarts: s.restarts,
                    exit_status: s.exit_status,
                    enabled: !self.dir.is_disabled(&services[i].name),
                }
            })
            .collect();
        (entries, more)
    }
}

/// Serves `Services` on `end` until it closes.
pub fn serve(dir: Rc<Dir>, end: ServerEnd<Services>) {
    let spawner = dir.spawner().clone();
    spawner.spawn(async move {
        let server = ServicesAsyncServer::new(end, dir.reactor().clone());
        while let Ok(ServicesRequest::List(q, r)) = server.next().await {
            let (entries, more) = match dir.listing() {
                Some(listing) => listing(q.after.as_deref()),
                None => (Vec::new(), false),
            };
            let _ = r.reply(server.server(), ServicesListResponse { entries, more });
        }
    });
}
