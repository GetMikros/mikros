//! Starting services through the process manager's `Launcher`.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_manifest::{PRIORITY_DEFAULT, Service};
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherAsyncClient, SpawnRequest, StartupHandle,
};
use mikros_rt::interfaces::mikros::service::{Provider, ProviderClient};
use mikros_rt::sys;

/// `user` in a Spawn request: the caller's own user.
const CALLER_USER: u32 = u32::MAX;
const PAGE_SIZE: u64 = 4096;

/// A service that started.
pub struct Started {
    pub pid: u32,
    #[expect(dead_code, reason = "watched once services are restarted")]
    pub process: OwnedHandle,
    /// The service manager's end of the service's `Provider` channel.
    pub provider: Rc<ProviderClient>,
}

/// Starts `service` with app ID `system.<name>`, the service manager's
/// own user, a new `Provider` channel and the startup handles `handles`.
pub async fn launch(
    launcher: &LauncherAsyncClient,
    service: &Service,
    mut handles: Vec<StartupHandle>,
) -> Result<Started, String> {
    if service.priority > 31 {
        return Err(format!("its priority {} is above 31", service.priority));
    }
    // TODO: give a PRIORITY resource for priorities 17 to 31, and the
    // boot handles that a manifest lists.
    if service.priority > PRIORITY_DEFAULT {
        return Err("priorities above 16 are not given yet".into());
    }
    if !service.boot_handles.is_empty() {
        return Err("boot handles are not given to services yet".into());
    }
    let (provider, server) = endpoints::<Provider>().map_err(|e| format!("{e:?}"))?;
    handles.push(StartupHandle {
        role: HandleRole::Provider,
        id: 0,
        handle: server.into_handle(),
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
        }),
        Ok(Err(status)) => Err(format!("Spawn failed: {status:?}")),
        Err(e) => Err(format!("Spawn failed: {e:?}")),
    }
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
