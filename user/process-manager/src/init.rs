//! Serving init: registering it as process ID 1 and starting the
//! service manager.

use crate::launcher;
use crate::records::{Record, Records};
use crate::start::{self, Transfer, duplicate};
use alloc::rc::Rc;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    HandleRole, InitBootstrap, InitBootstrapAsyncServer, InitBootstrapRegisterResponse,
    InitBootstrapRequest, InitBootstrapStartServiceManagerResponse, Launcher, ManagerStartup,
    StartupHandle,
};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, Directory, ProcessDirectory, ProcessDirectoryAsyncClient,
};
use mikros_rt::sys;
use process_manager::bootfs::Fs;

const SAME_RIGHTS: u32 = 1 << 31;
const INFO_BASIC: u32 = 1;
const INFO_BASIC_SIZE: usize = 24;
/// The service manager's program, and its path in its own namespace.
const SERVICE_MANAGER: &str = "usr/bin/service-manager";
const SERVICE_MANAGER_PATH: &str = "/usr/bin/service-manager";
const SERVICE_MANAGER_APP: &str = "system.service-manager";
/// The identity of the process manager's own directory.
const PROCESS_MANAGER_APP: &str = "system.process-manager";
/// The IDs of the boot handles that init passes to the service manager.
const BOOT_HANDLE_IDS: core::ops::RangeInclusive<u32> = 1..=5;
/// The `USER` startup handle ID of the service manager's control
/// endpoint.
const CONTROL_ID: u32 = 0;
const INIT_PID: u32 = 1;

/// The process manager's state while it serves init.
pub struct Manager {
    /// The log handle, which the runtime's logging writes to.
    _log: OwnedHandle,
    budget: OwnedHandle,
    clock_page: OwnedHandle,
    init_process: Option<OwnedHandle>,
    init: Option<ServerEnd<InitBootstrap>>,
    records: Records,
    fs: Rc<Fs>,
    reactor: Reactor,
    spawner: Spawner,
    /// The process ID of the service manager, once its start began.
    service_manager: Option<u32>,
    /// The control endpoint of the service manager, once it started.
    control: Option<ProcessDirectoryAsyncClient>,
    /// The process manager's own directory, once it is bound.
    directory: Option<ClientEnd<Directory>>,
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
            ..
        } = startup;
        let fs = Fs::new(process.raw(), &boot_image, reactor.clone(), spawner.clone())?;
        Ok(Manager {
            _log: log,
            budget,
            clock_page,
            init_process: Some(init_process),
            init: Some(init_bootstrap),
            records: Records::default(),
            fs,
            reactor,
            spawner,
            service_manager: None,
            control: None,
            directory: None,
        })
    }

    /// Serves init's requests until init closes its end.
    pub async fn serve_init(&mut self) {
        let Some(end) = self.init.take() else {
            return;
        };
        let server = InitBootstrapAsyncServer::new(end, self.reactor.clone());
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
            app_id: "system.init".into(),
            user: 0,
            login_session: 0,
            budget: duplicate(self.budget.raw(), SAME_RIGHTS)?,
            path: "init".into(),
            process,
        };
        if !self.records.insert(INIT_PID, record) {
            return Err(Status::BadState);
        }
        Ok(InitBootstrapRegisterResponse {})
    }

    /// Starts the service manager, once, after init has registered.
    async fn start_service_manager(
        &mut self,
        handles: alloc::vec::Vec<StartupHandle>,
    ) -> Result<InitBootstrapStartServiceManagerResponse, Status> {
        if self.init_process.is_some() || self.service_manager.is_some() {
            return Err(Status::BadState);
        }
        let pid = self.records.next_pid();
        self.service_manager = Some(pid);
        let boot_handle =
            |h: &StartupHandle| h.role == HandleRole::User && BOOT_HANDLE_IDS.contains(&h.id);
        if !handles.iter().all(boot_handle) {
            return Err(Status::InvalidArgs);
        }
        let started = start::load(
            &self.fs,
            self.budget.raw(),
            SERVICE_MANAGER,
            "service-manager",
        )?;
        let bound = match self.transfer(&started, pid, handles).await {
            Ok(control) => bind_own_directory(&control).await.map(|own| (control, own)),
            Err(status) => Err(status),
        };
        match bound {
            Ok((control, own)) => {
                self.control = Some(control);
                self.directory = Some(own);
            }
            Err(status) => {
                started.kill();
                return Err(status);
            }
        }
        let record = Record {
            koid: koid(&started.process)?,
            parent: INIT_PID,
            app_id: SERVICE_MANAGER_APP.into(),
            user: 0,
            login_session: 0,
            budget: duplicate(self.budget.raw(), SAME_RIGHTS)?,
            path: SERVICE_MANAGER_PATH.into(),
            process: duplicate(started.process.raw(), SAME_RIGHTS)?,
        };
        if !self.records.insert(pid, record) {
            return Err(Status::BadState);
        }
        Ok(InitBootstrapStartServiceManagerResponse {
            pid,
            process: duplicate(started.process.raw(), SAME_RIGHTS)?,
        })
    }

    /// Sends the service manager's startup transfer, with its control
    /// endpoint ahead of init's `handles`. Its directory is bound over
    /// that endpoint before `Complete`. Returns the control endpoint.
    async fn transfer(
        &self,
        started: &start::Started,
        pid: u32,
        init_handles: alloc::vec::Vec<StartupHandle>,
    ) -> Result<ProcessDirectoryAsyncClient, Status> {
        let channel = |_| Status::NoResources;
        let (control, control_end) = endpoints::<ProcessDirectory>().map_err(channel)?;
        let (launcher, launcher_end) = endpoints::<Launcher>().map_err(channel)?;
        let (directory, directory_end): (ClientEnd<Directory>, _) =
            endpoints::<Directory>().map_err(channel)?;
        self.spawner
            .spawn(launcher::serve(launcher_end, pid, self.reactor.clone()));
        let mut handles = alloc::vec![StartupHandle {
            role: HandleRole::User,
            id: CONTROL_ID,
            handle: control_end.into_handle(),
        }];
        handles.extend(init_handles);
        started.send(Transfer {
            pid,
            parent: INIT_PID,
            user: 0,
            app_id: SERVICE_MANAGER_APP.into(),
            args: &[SERVICE_MANAGER_PATH],
            handles,
            launcher,
            directory,
            clock_page: self.clock_page.raw(),
            root: start::root(&self.fs)?,
        })?;
        let control = ProcessDirectoryAsyncClient::new(control, self.reactor.clone());
        let identity = ClientIdentity {
            pid,
            user: 0,
            app_id: SERVICE_MANAGER_APP.into(),
        };
        bind(&control, identity, directory_end).await?;
        started.complete()?;
        Ok(control)
    }
}

/// Binds `directory` to `identity` over the service manager's control
/// endpoint, and waits for the acknowledgment.
async fn bind(
    control: &ProcessDirectoryAsyncClient,
    identity: ClientIdentity,
    directory: ServerEnd<Directory>,
) -> Result<(), Status> {
    match control.bind(identity, directory).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// The process manager's own `Directory` connection, bound over the
/// service manager's control endpoint.
async fn bind_own_directory(
    control: &ProcessDirectoryAsyncClient,
) -> Result<ClientEnd<Directory>, Status> {
    let (own, own_end) = endpoints::<Directory>().map_err(|_| Status::NoResources)?;
    let identity = ClientIdentity {
        pid: 0,
        user: 0,
        app_id: PROCESS_MANAGER_APP.into(),
    };
    bind(control, identity, own_end).await?;
    Ok(own)
}

/// The kernel object ID of `handle`'s object.
fn koid(handle: &OwnedHandle) -> Result<u64, Status> {
    let mut record = [0u8; INFO_BASIC_SIZE];
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
    if status != mikros_rt::OK {
        return Err(Status::from_raw(status as i32));
    }
    Ok(u64::from_le_bytes(record[..8].try_into().expect("8 bytes")))
}
