//! The standard output and error of services, which go to the log
//! service: the service manager connects to its `mikros.log.Sources` by
//! giving the server end straight to the log service's `Provider`,
//! without the directory's permission check, which only the service
//! manager may do (MIKSPEC-0063.R13), and keeps the streams of the
//! services that start before the log service runs until it does.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::log::{Sources as SourcesProtocol, SourcesAsyncClient};
use mikros_rt::interfaces::mikros::service::{ClientIdentity, ProviderClient};

/// The log service's protocol for the streams.
pub const SOURCES: &str = "mikros.log.Sources";

/// A service's streams, with its pid and app ID.
struct Source {
    pid: u32,
    app_id: String,
    output: OwnedHandle,
    error: OwnedHandle,
}

pub struct Sources {
    me: ClientIdentity,
    reactor: Reactor,
    spawner: Spawner,
    /// The connection to the log service, while it runs.
    log: RefCell<Option<Rc<SourcesAsyncClient>>>,
    /// The streams kept until the log service runs.
    kept: RefCell<Vec<Source>>,
}

impl Sources {
    /// Streams that go to the log service of the service manager `me`.
    pub fn new(me: ClientIdentity, reactor: Reactor, spawner: Spawner) -> Rc<Sources> {
        Rc::new(Sources {
            me,
            reactor,
            spawner,
            log: RefCell::new(None),
            kept: RefCell::new(Vec::new()),
        })
    }

    /// Gives the log service the other ends of the standard output and
    /// error of the service `pid` with `app_id`, or keeps them until the
    /// log service runs.
    pub fn add(self: &Rc<Self>, pid: u32, app_id: String, output: OwnedHandle, error: OwnedHandle) {
        let source = Source {
            pid,
            app_id,
            output,
            error,
        };
        let log = self.log.borrow().clone();
        match log {
            Some(log) => self.send(log, source),
            None => self.kept.borrow_mut().push(source),
        }
    }

    fn send(self: &Rc<Self>, log: Rc<SourcesAsyncClient>, source: Source) {
        let this = self.clone();
        self.spawner.spawn(async move {
            let Source {
                pid,
                app_id,
                output,
                error,
            } = source;
            let added = log.add(pid, app_id.clone(), output, error).await;
            if !matches!(added, Ok(Ok(_))) {
                mikros_rt::log!(
                    Error,
                    "the log service did not take the output of {app_id} (pid {pid})"
                );
                // The log service ended; streams wait for the next one.
                let mut current = this.log.borrow_mut();
                if current.as_ref().is_some_and(|l| Rc::ptr_eq(l, &log)) {
                    *current = None;
                }
            }
        });
    }

    /// Connects to the log service, which runs with `provider`, and gives
    /// it the streams that were kept.
    pub fn log_runs(self: &Rc<Self>, provider: &ProviderClient) {
        let Ok((client, server)) = endpoints::<SourcesProtocol>() else {
            return;
        };
        let me = ClientIdentity {
            pid: self.me.pid,
            user: self.me.user,
            app_id: self.me.app_id.clone(),
        };
        if provider
            .connect(SOURCES.into(), None, server.into_handle(), me)
            .is_err()
        {
            mikros_rt::log!(Error, "connecting to the log service's {SOURCES} failed");
            return;
        }
        let log = Rc::new(SourcesAsyncClient::new(client, self.reactor.clone()));
        *self.log.borrow_mut() = Some(log.clone());
        for source in self.kept.take() {
            self.send(log.clone(), source);
        }
    }
}
