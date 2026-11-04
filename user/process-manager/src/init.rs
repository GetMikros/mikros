//! Serving init: registering it as process ID 1 and starting the
//! service manager.

use crate::boot;
use crate::records::{Record, Records};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    HandleRole, InitBootstrapRegisterResponse, InitBootstrapRequest, InitBootstrapServer,
    InitBootstrapStartServiceManagerResponse, ManagerStartup, StartupHandle,
};
use mikros_rt::sys;

const SAME_RIGHTS: u32 = 1 << 31;
const INFO_BASIC: u32 = 1;
const INFO_BASIC_SIZE: usize = 24;
/// The boot image member of the service manager.
const SERVICE_MANAGER: &[u8] = b"usr/bin/service-manager";
/// The IDs of the boot handles that init passes to the service manager.
const BOOT_HANDLE_IDS: core::ops::RangeInclusive<u32> = 1..=5;

/// The process manager's state while it serves init.
pub struct Manager {
    process: OwnedHandle,
    budget: OwnedHandle,
    boot_image: OwnedHandle,
    init_process: Option<OwnedHandle>,
    init: Option<InitBootstrapServer>,
    records: Records,
    service_manager_requested: bool,
}

impl Manager {
    pub fn new(startup: ManagerStartup) -> Manager {
        let ManagerStartup {
            process,
            init_process,
            budget,
            boot_image,
            init_bootstrap,
            ..
        } = startup;
        Manager {
            process,
            budget,
            boot_image,
            init_process: Some(init_process),
            init: Some(InitBootstrapServer::new(init_bootstrap)),
            records: Records::default(),
            service_manager_requested: false,
        }
    }

    /// Serves init's requests until init closes its end.
    pub fn serve_init(&mut self) {
        let Some(server) = self.init.take() else {
            return;
        };
        loop {
            match server.next(i64::MAX) {
                Ok(InitBootstrapRequest::Register(_, responder)) => {
                    let result = self.register();
                    let _ = responder.reply(&server, result);
                }
                Ok(InitBootstrapRequest::StartServiceManager(request, responder)) => {
                    let result = self.start_service_manager(request.handles);
                    let _ = responder.reply(&server, result);
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
            budget: duplicate(&self.budget)?,
            path: "init".into(),
            process,
        };
        if !self.records.insert(1, record) {
            return Err(Status::BadState);
        }
        Ok(InitBootstrapRegisterResponse {})
    }

    /// Starts the service manager, once, after init has registered.
    fn start_service_manager(
        &mut self,
        handles: alloc::vec::Vec<StartupHandle>,
    ) -> Result<InitBootstrapStartServiceManagerResponse, Status> {
        if self.init_process.is_some() || self.service_manager_requested {
            return Err(Status::BadState);
        }
        self.service_manager_requested = true;
        let boot_handle =
            |h: &StartupHandle| h.role == HandleRole::User && BOOT_HANDLE_IDS.contains(&h.id);
        if !handles.iter().all(boot_handle) {
            return Err(Status::InvalidArgs);
        }
        let found = boot::with_image(self.process.raw(), &self.boot_image, |image| {
            mikros_ustar::find(image, SERVICE_MANAGER).is_some()
        })
        .map_err(|s| Status::from_raw(s as i32))?;
        if !found {
            return Err(Status::NotFound);
        }
        // TODO: load and start the service manager once the boot file
        // system and the startup transfer are served.
        Err(Status::NotSupported)
    }
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

/// A second handle to `handle`'s object with the same rights.
fn duplicate(handle: &OwnedHandle) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle.raw(), SAME_RIGHTS, &mut out) };
    if status != mikros_rt::OK {
        return Err(Status::from_raw(status as i32));
    }
    // SAFETY: the handle was just created for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}
