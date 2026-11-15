//! `mikros.service.ServiceControl`, which the service manager serves to
//! processes with `admin.services.control`, for which the directory has
//! already asked: `Start`, `Stop` and `Restart` of a service by name.

use crate::directory::{Call, Dir};
use crate::supervisor::Supervisor;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::{
    ServiceControl, ServiceControlAsyncServer, ServiceControlRequest,
    ServiceControlRestartResponse, ServiceControlStartResponse, ServiceControlStopResponse,
};

/// Serves `ServiceControl` on `end` until it closes. Each call is
/// answered before the next is read.
pub fn serve(dir: Rc<Dir>, end: ServerEnd<ServiceControl>) {
    let spawner = dir.spawner().clone();
    spawner.spawn(async move {
        let server = ServiceControlAsyncServer::new(end, dir.reactor().clone());
        let manage = |call, name: String| {
            let manage = dir.manage();
            async move {
                match manage {
                    Some(manage) => manage(call, name).await,
                    None => Err(Status::BadState),
                }
            }
        };
        while let Ok(request) = server.next().await {
            let _ = match request {
                ServiceControlRequest::Start(q, r) => {
                    let done = manage(Call::Start, q.name).await;
                    r.reply(
                        server.server(),
                        done.map(|()| ServiceControlStartResponse {}),
                    )
                }
                ServiceControlRequest::Stop(q, r) => {
                    let done = manage(Call::Stop, q.name).await;
                    r.reply(
                        server.server(),
                        done.map(|()| ServiceControlStopResponse {}),
                    )
                }
                ServiceControlRequest::Restart(q, r) => {
                    let done = manage(Call::Restart, q.name).await;
                    r.reply(
                        server.server(),
                        done.map(|()| ServiceControlRestartResponse {}),
                    )
                }
            };
        }
    });
}

impl Supervisor {
    /// Does `call` for the service named `name`. Every call fails with
    /// `NOT_FOUND` for a name without a valid manifest, and with
    /// `BAD_STATE` while the machine shuts down or the service is being
    /// started. `Stop` and `Restart` fail with `NOT_SUPPORTED` for a
    /// service of the boot image, which keeps running.
    pub async fn manage(self: &Rc<Self>, call: Call, name: &str) -> Result<(), Status> {
        let i = self.position(name).ok_or(Status::NotFound)?;
        if call != Call::Start && i < self.boot_services {
            return Err(Status::NotSupported);
        }
        if self.stopping.get() || self.starting.borrow().contains(&i) {
            return Err(Status::BadState);
        }
        let pid = self.pid(i);
        match (call, pid) {
            (Call::Start, Some(_)) | (Call::Stop, None) => return Err(Status::BadState),
            (Call::Stop | Call::Restart, Some(pid)) => {
                // Held, the service is not restarted when it ends.
                self.held.borrow_mut().insert(i);
                self.stop(i, pid).await;
                if call == Call::Stop {
                    return Ok(());
                }
            }
            (Call::Start | Call::Restart, None) => {}
        }
        // Nothing else starts it from here on: neither a connection to an
        // idle service nor another call.
        self.starting.borrow_mut().insert(i);
        self.held.borrow_mut().remove(&i);
        self.restarts.borrow_mut().remove(&i);
        let started = self.start_with_after(i, Vec::new()).await;
        self.starting.borrow_mut().remove(&i);
        match started {
            true => Ok(()),
            false => Err(Status::Internal),
        }
    }

    /// The process ID of service `i` while it runs.
    fn pid(&self, i: usize) -> Option<u32> {
        let running = self.running.borrow();
        running.iter().find(|&&(j, _)| j == i).map(|&(_, pid)| pid)
    }
}
