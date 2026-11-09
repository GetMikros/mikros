//! Starting services through the process manager's `Launcher`, and
//! seeing them end.

use crate::boot::BootHandles;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_manifest::Service;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherAsyncClient, SpawnRequest, StartupHandle,
};
use mikros_rt::interfaces::mikros::service::{Provider, ProviderClient};
use mikros_rt::sys;

/// `user` in a Spawn request: the caller's own user.
const CALLER_USER: u32 = u32::MAX;
const PAGE_SIZE: u64 = 4096;
const TERMINATED: u32 = 1 << 4;
const INFO_PROCESS: u32 = 2;
const INFO_PROCESS_SIZE: usize = 0x28;

/// A service that started.
pub struct Started {
    pub pid: u32,
    pub process: OwnedHandle,
    /// The service manager's end of the service's `Provider` channel.
    pub provider: Rc<ProviderClient>,
    /// The other ends of the service's standard output and error, for
    /// the log service, until they are taken.
    pub streams: Option<(OwnedHandle, OwnedHandle)>,
}

/// The `id` of standard output and error among the startup handles:
/// descriptors 1 and 2, of the pipe kind 2 (MIKSPEC-0058.R3).
const STDOUT_ID: u32 = 1 + (2 << 28);
const STDERR_ID: u32 = 2 + (2 << 28);

/// A new stream: the service's end and the other.
fn stream() -> Result<(OwnedHandle, OwnedHandle), String> {
    let (mut ours, mut theirs) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    let status = unsafe { sys::stream_create(0, &mut theirs, &mut ours) };
    if status != mikros_rt::OK {
        return Err(format!("creating a stream: {status}"));
    }
    // SAFETY: the kernel just created both handles for us.
    Ok(unsafe { (OwnedHandle::from_raw(theirs), OwnedHandle::from_raw(ours)) })
}

/// Starts `service` with app ID `system.<name>`, the service manager's
/// own user, a new `Provider` channel, a stream each as its standard
/// output and error, the boot handles its manifest lists and the
/// `PRIORITY` resource its priority needs, from `boot`, and the startup
/// handles `handles`.
pub async fn launch(
    launcher: &LauncherAsyncClient,
    boot: &BootHandles,
    service: &Service,
    mut handles: Vec<StartupHandle>,
) -> Result<Started, String> {
    handles.extend(boot.priority(service.priority)?);
    handles.extend(boot.give(&service.boot_handles)?);
    let (provider, server) = endpoints::<Provider>().map_err(|e| format!("{e:?}"))?;
    handles.push(StartupHandle {
        role: HandleRole::Provider,
        id: 0,
        handle: server.into_handle(),
    });
    let (output, output_end) = stream()?;
    let (error, error_end) = stream()?;
    handles.push(StartupHandle {
        role: HandleRole::Stdout,
        id: STDOUT_ID,
        handle: output,
    });
    handles.push(StartupHandle {
        role: HandleRole::Stderr,
        id: STDERR_ID,
        handle: error,
    });
    let request = SpawnRequest {
        path: service.binary.clone(),
        arguments: arguments(&service.binary).map_err(|s| format!("arguments: {s}"))?,
        handles,
        app_id: Some(format!("system.{}", service.name)),
        user: CALLER_USER,
        login_session: false,
    };
    match launcher.spawn(request).await {
        Ok(Ok(spawned)) => Ok(Started {
            pid: spawned.pid,
            process: spawned.process,
            provider: Rc::new(ProviderClient::new(provider)),
            streams: Some((output_end, error_end)),
        }),
        Ok(Err(status)) => Err(format!("Spawn failed: {status:?}")),
        Err(e) => Err(format!("Spawn failed: {e:?}")),
    }
}

/// Waits for `process` to end, and returns its exit code and the signal
/// that ended it, or `None` when they cannot be read.
pub async fn exited(reactor: &Reactor, process: &OwnedHandle) -> Option<(i64, u32)> {
    reactor.signals(process.raw(), TERMINATED).await.ok()?;
    let mut record = [0u8; INFO_PROCESS_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status = unsafe {
        sys::object_get_info(
            process.raw(),
            INFO_PROCESS,
            record.as_mut_ptr(),
            record.len() as u64,
            &mut len,
        )
    };
    if status != mikros_rt::OK {
        return None;
    }
    let code = i64::from_le_bytes(record[..8].try_into().ok()?);
    let signal = u32::from_le_bytes(record[0x10..0x14].try_into().ok()?);
    Some((code, signal))
}

/// A new memory object of whole pages that starts with `bytes`.
pub fn memory(bytes: &[u8]) -> Result<OwnedHandle, sys::Status> {
    let size = (bytes.len() as u64).max(1).next_multiple_of(PAGE_SIZE);
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::mem_create(size, 0, &mut mem) };
    if status != mikros_rt::OK {
        return Err(status);
    }
    // SAFETY: the kernel just created the handle for us.
    let mem = unsafe { OwnedHandle::from_raw(mem) };
    // SAFETY: the pointer and length describe `bytes`.
    let status = unsafe { sys::mem_write(mem.raw(), 0, bytes.as_ptr(), bytes.len() as u64) };
    if status != mikros_rt::OK {
        return Err(status);
    }
    Ok(mem)
}

/// An `arguments` memory object that holds only `program`.
fn arguments(program: &str) -> Result<OwnedHandle, sys::Status> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(program.as_bytes());
    bytes.push(0);
    memory(&bytes)
}
