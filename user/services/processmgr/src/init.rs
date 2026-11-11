//! Serving init: registering it as process ID 1 and starting the
//! service manager.

use crate::namespace::{Namespaces, Programs};
use crate::processes::{NewProcess, Shared, bind, koid};
use crate::records::{INIT_PID, Record};
use crate::start::{self, duplicate};
use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    HandleRole, InitBootstrap, InitBootstrapAsyncServer, InitBootstrapRegisterResponse,
    InitBootstrapRequest, InitBootstrapStartServiceManagerResponse, ManagerStartup, StartupHandle,
};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, Directory, DirectoryAsyncClient, ProcessDirectory, ProcessDirectoryAsyncClient,
};
use mikros_rt::interfaces::mikros::time::{Clock, ClockAsyncClient};
use mikros_rt::sys;
use processmgr::bootfs::Fs;

const SAME_RIGHTS: u32 = 1 << 31;
/// The time service's protocol.
const CLOCK: &str = "mikros.time.Clock";
/// The service manager's program, which init's namespace resolves.
const SERVICE_MANAGER_PATH: &str = "/usr/bin/servicemgr";
const SERVICE_MANAGER_APP: &str = "system.servicemgr";
/// The identity of the process manager's own directory.
const PROCESS_MANAGER_APP: &str = "system.processmgr";
/// The IDs of the boot handles that init passes to the service manager.
const BOOT_HANDLE_IDS: core::ops::RangeInclusive<u32> = 1..=5;
/// The `USER` startup handle ID of the service manager's control
/// endpoint.
const CONTROL_ID: u32 = 0;
/// The process group and session of init.
const INIT_GROUP: u32 = 1;

/// The process manager's state while it serves init.
pub struct Manager {
    /// The log handle, which the runtime's logging writes to.
    _log: OwnedHandle,
    /// The process manager's own process, which the runtime's heap maps
    /// its larger blocks into.
    _process: OwnedHandle,
    shared: Rc<Shared>,
    init_process: Option<OwnedHandle>,
    init: Option<ServerEnd<InitBootstrap>>,
    /// The process ID of the service manager, once its start began.
    service_manager: Option<u32>,
}

impl Manager {
    /// The manager of the processes of `startup`, which serves the boot
    /// file system on the executor of `reactor` and `spawner`.
    pub fn new(
        startup: ManagerStartup,
        reactor: Reactor,
        spawner: Spawner,
    ) -> Result<Manager, sys::Status> {
        let ManagerStartup {
            process,
            init_process,
            budget,
            boot_image,
            clock_page,
            log,
            init_bootstrap,
            command_line,
            ..
        } = startup;
        let fs = Fs::new(process.raw(), &boot_image, reactor.clone(), spawner.clone())?;
        let namespaces = Rc::new(Namespaces::new(&command_line));
        let mut own = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::handle_duplicate(process.raw(), SAME_RIGHTS, &mut own) };
        if status != mikros_rt::OK {
            return Err(status);
        }
        // SAFETY: the kernel just created the handle for us.
        let own = unsafe { OwnedHandle::from_raw(own) };
        Ok(Manager {
            _log: log,
            _process: process,
            shared: Shared::new(fs, namespaces, budget, clock_page, own, reactor, spawner),
            init_process: Some(init_process),
            init: Some(init_bootstrap),
            service_manager: None,
        })
    }

    /// Serves init's requests until init closes its end.
    pub async fn serve_init(&mut self) {
        let Some(end) = self.init.take() else {
            return;
        };
        let server = InitBootstrapAsyncServer::new(end, self.shared.reactor.clone());
        loop {
            match server.next().await {
                Ok(InitBootstrapRequest::Register(_, responder)) => {
                    let result = self.register();
                    let _ = responder.reply(server.server(), result);
                }
                Ok(InitBootstrapRequest::StartServiceManager(request, responder)) => {
                    let result = self.start_service_manager(request.handles).await;
                    let _ = responder.reply(server.server(), result);
                }
                Err(_) => return,
            }
        }
    }

    /// Creates init's record, once.
    fn register(&mut self) -> Result<InitBootstrapRegisterResponse, Status> {
        let process = self.init_process.take().ok_or(Status::BadState)?;
        let record = Record {
            koid: koid(&process)?,
            parent: 0,
            process_group: INIT_GROUP,
            session: INIT_GROUP,
            app_id: "system.init".into(),
            user: 0,
            login_session: 0,
            budget: duplicate(self.shared.budget.raw(), SAME_RIGHTS)?,
            path: "init".into(),
            // init's programs come from the boot file system.
            programs: Rc::new(Programs::boot()),
            process,
            exit: None,
            stopped: None,
            continued: false,
        };
        if !self.shared.records.borrow_mut().insert(INIT_PID, record) {
            return Err(Status::BadState);
        }
        Ok(InitBootstrapRegisterResponse {})
    }

    /// Starts the service manager, once, after init has registered. Its
    /// control endpoint goes ahead of init's `handles`, and its own
    /// directory is bound over that endpoint before `Complete`.
    async fn start_service_manager(
        &mut self,
        init_handles: Vec<StartupHandle>,
    ) -> Result<InitBootstrapStartServiceManagerResponse, Status> {
        if self.init_process.is_some() || self.service_manager.is_some() {
            return Err(Status::BadState);
        }
        let boot_handle =
            |h: &StartupHandle| h.role == HandleRole::User && BOOT_HANDLE_IDS.contains(&h.id);
        if !init_handles.iter().all(boot_handle) {
            return Err(Status::InvalidArgs);
        }
        let pid = self
            .shared
            .records
            .borrow_mut()
            .reserve()
            .ok_or(Status::NoResources)?;
        self.service_manager = Some(pid);
        let (control, control_end) =
            endpoints::<ProcessDirectory>().map_err(|_| Status::NoResources)?;
        let control = ProcessDirectoryAsyncClient::new(control, self.shared.reactor.clone());
        let mut handles = alloc::vec![StartupHandle {
            role: HandleRole::User,
            id: CONTROL_ID,
            handle: control_end.into_handle(),
        }];
        handles.extend(init_handles);
        let new = NewProcess {
            path: SERVICE_MANAGER_PATH.into(),
            // As init's programs, it comes from the boot file system.
            programs: Rc::new(Programs::boot()),
            parent: INIT_PID,
            process_group: INIT_GROUP,
            session: INIT_GROUP,
            app_id: SERVICE_MANAGER_APP.into(),
            user: 0,
            login_session: 0,
            arguments: start::arguments(&[SERVICE_MANAGER_PATH])?,
            handles,
            service_manager: true,
            working_directory: "/".into(),
        };
        // The service manager may spawn as soon as its transfer is
        // complete, and its children are bound over the same endpoint.
        let control = Rc::new(control);
        *self.shared.control.borrow_mut() = Some(control.clone());
        self.shared.identity_setter.set(Some(pid));
        let started = match self.shared.start(pid, new, &control).await {
            Ok(process) => bind_own_directory(&control).await.map(|own| (process, own)),
            Err(status) => Err(status),
        };
        let (process, own) = match started {
            Ok(started) => started,
            Err(status) => {
                self.shared.identity_setter.set(None);
                *self.shared.control.borrow_mut() = None;
                if let Some(record) = self.shared.records.borrow().get(pid) {
                    // SAFETY: the call takes no pointers.
                    unsafe { sys::task_kill(record.process.raw()) };
                }
                return Err(status);
            }
        };
        // Namespaces come from profiles once tmpfs serves when running
        // from the boot image, and once the root file system is published
        // too otherwise, through a directory of the process manager's own.
        let shared = self.shared.clone();
        if let Ok(directory) = bind_own_directory(&control).await {
            self.shared.spawner.spawn(async move {
                let reactor = shared.reactor.clone();
                let directory = DirectoryAsyncClient::new(directory, reactor.clone());
                let namespaces = shared.namespaces.clone();
                match namespaces.runs_from_boot_image() {
                    true => {
                        namespaces
                            .prepare(&shared, directory, shared.fs.clone(), reactor)
                            .await
                    }
                    false => namespaces.prepare_root(&shared, directory, reactor).await,
                }
            });
        }
        // The EFI system partition the system booted from comes as
        // `/boot/efi` once its file system is published, through another
        // directory of the process manager's own, which waits for it.
        if let Ok(directory) = bind_own_directory(&control).await {
            let shared = self.shared.clone();
            self.shared.spawner.spawn(async move {
                let reactor = shared.reactor.clone();
                let directory = DirectoryAsyncClient::new(directory, reactor.clone());
                shared.namespaces.prepare_esp(directory, reactor).await;
            });
        }
        // `/Volumes` follows the instances of volumes through another
        // directory of the process manager's own.
        if let Ok(directory) = bind_own_directory(&control).await {
            let shared = self.shared.clone();
            self.shared.spawner.spawn(async move {
                let directory = DirectoryAsyncClient::new(directory, shared.reactor.clone());
                shared.volumes.clone().watch(directory).await;
            });
        }
        // Processes get the time page once the time service gives it,
        // through another directory of the process manager's own; none
        // waits for it.
        if let Ok(directory) = bind_own_directory(&control).await {
            let shared = self.shared.clone();
            self.shared.spawner.spawn(async move {
                let reactor = shared.reactor.clone();
                let directory = DirectoryAsyncClient::new(directory, reactor.clone());
                if let Some(page) = time_page(&directory, &reactor).await {
                    *shared.time_page.borrow_mut() = Some(page);
                }
            });
        }
        let shared = self.shared.clone();
        self.shared.spawner.spawn(async move {
            let reactor = shared.reactor.clone();
            shared
                .permissions
                .connect(own, &reactor, own_identity())
                .await;
        });
        Ok(InitBootstrapStartServiceManagerResponse { pid, process })
    }
}

/// The process manager's own `Directory` connection, bound over the
/// service manager's control endpoint.
async fn bind_own_directory(
    control: &ProcessDirectoryAsyncClient,
) -> Result<ClientEnd<Directory>, Status> {
    let (own, own_end) = endpoints::<Directory>().map_err(|_| Status::NoResources)?;
    bind(control, own_identity(), own_end).await?;
    Ok(own)
}

/// The identity of the process manager's own directory.
fn own_identity() -> ClientIdentity {
    ClientIdentity {
        pid: 0,
        user: 0,
        app_id: PROCESS_MANAGER_APP.into(),
    }
}

/// The time page of the time service's `mikros.time.Clock` through
/// `directory`, once the service gives it; None without a time service.
async fn time_page(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<OwnedHandle> {
    let (client, server) = endpoints::<Clock>().ok()?;
    let connected = directory
        .connect(CLOCK.into(), None, server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return None;
    }
    let clock = ClockAsyncClient::new(client, reactor.clone());
    Some(clock.get_time_page().await.ok()?.page)
}
