//! bootsvc on Mikros: it makes a `FIRMWARE` resource from its
//! `root_resource` boot handle, closes that handle, takes the booted
//! version from the command line of its `platform_info` boot handle, and
//! serves `mikros.boot.Attempts`, reading and writing `MikrosBoot` with
//! the firmware variable calls. The service manager lets only processes
//! with `system.boot_state.set` connect.

use crate::attempts::{self, ATTRIBUTES, NAME, Record};
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::boot::{
    Attempts, AttemptsAsyncServer, AttemptsGetStateResponse, AttemptsMarkGoodResponse,
    AttemptsRequest, AttemptsStartTryingResponse, BootState,
};
use mikros_rt::interfaces::mikros::service::{Provider, ProviderAsyncServer, ProviderRequest};
use mikros_rt::sys;
use std::os::mikros::startup;
use std::process::ExitCode;
use std::rc::Rc;

/// The roles and IDs of the startup handles: the provider, and the boot
/// handles `root_resource` and `platform_info`.
const PROVIDER: u32 = 4;
const USER: u32 = 16;
const ROOT_RESOURCE_ID: u32 = 1;
const PLATFORM_INFO_ID: u32 = 5;
const KIND_FIRMWARE: u32 = 10;
const ATTEMPTS: &str = "mikros.boot.Attempts";

/// The kernel's statuses that the variable calls give.
const NOT_SUPPORTED: sys::Status = -2;
const BUFFER_TOO_SMALL: sys::Status = -15;
const NOT_FOUND: sys::Status = -25;

/// Where the platform information holds the command line's offset and
/// length.
const CMDLINE_OFFSET: u64 = 0x48;
const CMDLINE_MAX: usize = 4096;

/// The startup handle with `role` and `id`, as mikros-rt's handle.
fn take(role: u32, id: u32) -> Option<OwnedHandle> {
    let handle = startup::take_handle(role, id)?;
    // SAFETY: the handle is ours, and is handed over once.
    Some(unsafe { OwnedHandle::from_raw(handle.into_raw_handle()) })
}

/// A `FIRMWARE` resource made from the `ROOT` resource `root`.
fn firmware(root: &OwnedHandle) -> Option<OwnedHandle> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::resource_create(root.raw(), KIND_FIRMWARE, 0, 0, 0, &mut out) };
    // SAFETY: the kernel just created the handle for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
}

/// The command line that the platform information `info` holds, or an
/// empty one when it cannot be read or is not UTF-8.
fn command_line(info: &OwnedHandle) -> String {
    let mut place = [0u8; 8];
    // SAFETY: the pointer describes `place`.
    let status = unsafe { sys::mem_read(info.raw(), CMDLINE_OFFSET, place.as_mut_ptr(), 8) };
    if status != mikros_rt::OK {
        return String::new();
    }
    let offset = u32::from_le_bytes(place[..4].try_into().expect("4 bytes"));
    let len =
        (u32::from_le_bytes(place[4..].try_into().expect("4 bytes")) as usize).min(CMDLINE_MAX);
    let mut bytes = vec![0u8; len];
    // SAFETY: the pointer describes `bytes`.
    let status = unsafe {
        sys::mem_read(
            info.raw(),
            u64::from(offset),
            bytes.as_mut_ptr(),
            len as u64,
        )
    };
    match status {
        mikros_rt::OK => String::from_utf8(bytes).unwrap_or_default(),
        _ => String::new(),
    }
}

/// The status of a variable call that failed with `status`.
fn failure(status: sys::Status) -> Status {
    match status {
        NOT_SUPPORTED => Status::NotSupported,
        _ => Status::Io,
    }
}

struct Boot {
    firmware: OwnedHandle,
    /// The version that `system=` names, if any.
    booted: Option<String>,
}

impl Boot {
    /// The booted version, or `NOT_SUPPORTED` without one.
    fn booted(&self) -> Result<&str, Status> {
        self.booted.as_deref().ok_or(Status::NotSupported)
    }

    /// `MikrosBoot`, or `None` when it is missing or not valid.
    fn read(&self) -> Result<Option<Record>, Status> {
        // Room for any variable that firmware_variable_set writes.
        let mut data = vec![0u8; 1024];
        let (mut actual, mut attributes) = (0, 0);
        // SAFETY: the pointers describe the name, `data` and the locals.
        let status = unsafe {
            sys::firmware_variable_get(
                self.firmware.raw(),
                NAME.as_ptr(),
                NAME.len() as u64,
                data.as_mut_ptr(),
                data.len() as u64,
                &mut actual,
                &mut attributes,
            )
        };
        match status {
            mikros_rt::OK => {
                let len = (actual as usize).min(data.len());
                Ok(Record::decode(attributes, &data[..len]))
            }
            // A variable larger than any valid one counts as missing.
            NOT_FOUND | BUFFER_TOO_SMALL => Ok(None),
            status => Err(failure(status)),
        }
    }

    fn write(&self, record: &Record) -> Result<(), Status> {
        let data = record.encode();
        // SAFETY: the pointers describe the name and `data`.
        let status = unsafe {
            sys::firmware_variable_set(
                self.firmware.raw(),
                NAME.as_ptr(),
                NAME.len() as u64,
                ATTRIBUTES,
                data.as_ptr(),
                data.len() as u64,
            )
        };
        match status {
            mikros_rt::OK => Ok(()),
            status => Err(failure(status)),
        }
    }

    fn state(&self) -> Result<BootState, Status> {
        let booted = self.booted()?;
        let state = attempts::state(booted, self.read()?.as_ref());
        Ok(BootState {
            version: state.version,
            good: state.good,
            tries_left: state.tries_left,
            fell_back: state.fell_back,
        })
    }

    fn mark_good(&self) -> Result<(), Status> {
        let booted = self.booted()?;
        match attempts::mark_good(booted, self.read()?.as_ref()) {
            Some(record) => self.write(&record),
            None => Ok(()),
        }
    }

    /// Writes the variable so that the bootloader counts the next boots
    /// of the booted version again.
    fn start_trying(&self) -> Result<(), Status> {
        match attempts::start_trying(self.booted()?) {
            Some(record) => self.write(&record),
            None => Ok(()),
        }
    }
}

/// Serves `Attempts` on `end`.
async fn serve(boot: Rc<Boot>, end: ServerEnd<Attempts>, reactor: Reactor) {
    let server = AttemptsAsyncServer::new(end, reactor);
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            AttemptsRequest::GetState(_, r) => r.reply(
                s,
                boot.state().map(|state| AttemptsGetStateResponse { state }),
            ),
            AttemptsRequest::MarkGood(_, r) => {
                r.reply(s, boot.mark_good().map(|()| AttemptsMarkGoodResponse {}))
            }
            AttemptsRequest::StartTrying(_, r) => r.reply(
                s,
                boot.start_trying().map(|()| AttemptsStartTryingResponse {}),
            ),
        };
    }
}

pub fn main() -> ExitCode {
    let (Some(provider), Some(root), Some(info)) = (
        take(PROVIDER, 0),
        take(USER, ROOT_RESOURCE_ID),
        take(USER, PLATFORM_INFO_ID),
    ) else {
        eprintln!("bootsvc: the provider or a boot handle is missing");
        return ExitCode::FAILURE;
    };
    let Some(firmware) = firmware(&root) else {
        eprintln!("bootsvc: creating a FIRMWARE resource failed");
        return ExitCode::FAILURE;
    };
    drop(root);
    let booted = attempts::booted(&command_line(&info)).map(String::from);
    drop(info);
    let boot = Rc::new(Boot { firmware, booted });
    let Ok(mut executor) = Executor::new() else {
        return ExitCode::FAILURE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let provider = ProviderAsyncServer::new(ServerEnd::<Provider>::new(provider), reactor.clone());
    executor.block_on(async {
        while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
            // Other protocols and named instances are not ours; dropping
            // the server end closes the connection.
            if c.instance.is_none() && c.protocol == ATTEMPTS {
                let end = ServerEnd::new(c.server);
                spawner.spawn(serve(boot.clone(), end, reactor.clone()));
            }
        }
    });
    ExitCode::SUCCESS
}
