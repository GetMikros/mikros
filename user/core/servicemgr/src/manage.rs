//! `mikros.service.ServiceControl`, which the service manager serves to
//! every process: `Start`, `Stop`, `Restart`, `Enable` and `Disable` of a
//! service by name, each for a caller with `admin.services.control`, for
//! which it asks, but `Start`, `Stop` and `Restart` of the caller's own
//! services and its components' (MIKSPEC-0036.R52); and
//! `mikros.service.PackageServices`, which it serves to the install
//! service, which tells it that the services of packages changed
//! (MIKSPEC-0036.R40).

use crate::directory::{Call, Dir, SERVICE_CONTROL_PERMISSION};
use crate::supervisor::Supervisor;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, PackageServices, PackageServicesAsyncServer, PackageServicesRequest,
    PackageServicesServicesChangedResponse, ServiceControl, ServiceControlAsyncServer,
    ServiceControlDisableResponse, ServiceControlEnableResponse, ServiceControlRequest,
    ServiceControlRestartResponse, ServiceControlStartResponse, ServiceControlStopResponse,
};

/// Serves `ServiceControl` to `caller` on `end` until it closes. Each
/// call is answered before the next is read; once the broker allowed
/// `admin.services.control`, it is not asked again on this connection.
pub fn serve(dir: Rc<Dir>, end: ServerEnd<ServiceControl>, caller: ClientIdentity) {
    let spawner = dir.spawner().clone();
    spawner.spawn(async move {
        let server = ServiceControlAsyncServer::new(end, dir.reactor().clone());
        let admin = core::cell::Cell::new(false);
        let manage = |call, name: String| {
            let manage = dir.manage();
            let (dir, caller, admin) = (&dir, &caller, &admin);
            async move {
                let own = matches!(call, Call::Start | Call::Stop | Call::Restart)
                    && dir.owns_service(caller, &name).await;
                if !own && !admin.get() {
                    let reason = alloc::format!("control the service {name}");
                    if !dir
                        .request(caller, SERVICE_CONTROL_PERMISSION, reason)
                        .await
                    {
                        return Err(Status::AccessDenied);
                    }
                    admin.set(true);
                }
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
                ServiceControlRequest::Enable(q, r) => {
                    let done = manage(Call::Enable, q.name).await;
                    r.reply(
                        server.server(),
                        done.map(|()| ServiceControlEnableResponse {}),
                    )
                }
                ServiceControlRequest::Disable(q, r) => {
                    let done = manage(Call::Disable, q.name).await;
                    r.reply(
                        server.server(),
                        done.map(|()| ServiceControlDisableResponse {}),
                    )
                }
            };
        }
    });
}

/// Serves `PackageServices` to the install service on `end` until it
/// closes: `ServicesChanged` reads the services of packages again.
pub fn serve_packages(dir: Rc<Dir>, end: ServerEnd<PackageServices>) {
    let spawner = dir.spawner().clone();
    spawner.spawn(async move {
        let server = PackageServicesAsyncServer::new(end, dir.reactor().clone());
        while let Ok(PackageServicesRequest::ServicesChanged(_, r)) = server.next().await {
            let done = match dir.manage() {
                Some(manage) => manage(Call::ServicesChanged, String::new()).await,
                None => Err(Status::BadState),
            };
            let _ = r.reply(
                server.server(),
                done.map(|()| PackageServicesServicesChangedResponse {}),
            );
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
        if call == Call::ServicesChanged {
            return crate::packages::changed(self).await;
        }
        if matches!(call, Call::Enable | Call::Disable) {
            return self.enable(name, call == Call::Enable).await;
        }
        let i = self.position(name).ok_or(Status::NotFound)?;
        if self.removed.borrow().contains(&i) {
            return Err(Status::NotFound);
        }
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
            // Answered above.
            (Call::ServicesChanged | Call::Enable | Call::Disable, _) => return Ok(()),
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

    /// Enables the service `name` when `enable`, and disables it
    /// otherwise, without starting or stopping it (MIKSPEC-0036.R46): it
    /// fails with `BAD_STATE` before the root file system is available,
    /// `NOT_FOUND` for a name without a valid manifest, and `NOT_SUPPORTED`
    /// for a service of the boot image.
    async fn enable(&self, name: &str, enable: bool) -> Result<(), Status> {
        let state = self.state.borrow().clone().ok_or(Status::BadState)?;
        let i = self.position(name).ok_or(Status::NotFound)?;
        if self.removed.borrow().contains(&i) {
            return Err(Status::NotFound);
        }
        if i < self.boot_services {
            return Err(Status::NotSupported);
        }
        let mut disabled = self.dir.disabled();
        let changed = match enable {
            true => disabled.remove(name),
            false => disabled.insert(String::from(name)),
        };
        if changed {
            crate::disabled::write(&state, &self.reactor, &disabled).await?;
            self.dir.set_disabled(disabled);
        }
        Ok(())
    }

    /// The process ID of service `i` while it runs.
    pub fn pid(&self, i: usize) -> Option<u32> {
        let running = self.running.borrow();
        running.iter().find(|&&(j, _)| j == i).map(|&(_, pid)| pid)
    }
}
