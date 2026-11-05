//! Starting the permission broker, before every other service.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use mikros_manifest::{PRIORITY_DEFAULT, Service};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient};
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherAsyncClient, SpawnRequest, StartupHandle,
};
use mikros_rt::interfaces::mikros::service::{ClientIdentity, Provider, ProviderClient};
use mikros_rt::sys;

/// The broker's service name.
pub const NAME: &str = "permissions";
const PROTOCOL: &str = "mikros.permissions.Broker";
/// The app ID of the process manager, and the permissions the service
/// manager registers for it.
const PROCESS_MANAGER: &str = "system.process-manager";
const PROCESS_MANAGER_PERMISSIONS: &[&str] = &[
    "device.registry",
    "filesystem.mount",
    "filesystem.compose",
    "permissions.check",
];
/// `user` in a Spawn request: the caller's own user.
const CALLER_USER: u32 = u32::MAX;
/// The `id` of the manifest among the broker's `USER` startup handles.
const MANIFEST_ID: u32 = 0;
const PAGE_SIZE: u64 = 4096;
const SEAL_WRITE: u32 = 1 << 3;
/// The rights the broker's manifest is given: those of a new memory
/// object without `WRITE`.
const MANIFEST_RIGHTS: u32 = DUPLICATE | TRANSFER | READ | MAP | INSPECT;
const DUPLICATE: u32 = 1 << 0;
const TRANSFER: u32 = 1 << 1;
const READ: u32 = 1 << 2;
const MAP: u32 = 1 << 5;
const INSPECT: u32 = 1 << 9;

/// The running broker: the service manager's end of its `Provider`
/// channel, and its own connection to it.
#[expect(
    dead_code,
    reason = "used to forward connections and register services later"
)]
pub struct Running {
    pub pid: u32,
    pub provider: ProviderClient,
    pub broker: BrokerAsyncClient,
}

/// Starts the broker of `service`, whose manifest is `manifest`,
/// connects to it as the service manager `me`, and registers the
/// process manager's permissions.
pub async fn start(
    launcher: &LauncherAsyncClient,
    reactor: &Reactor,
    me: ClientIdentity,
    service: &Service,
    manifest: &[u8],
) -> Result<Running, String> {
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
    let sealed = sealed_copy(manifest).map_err(|s| format!("sealing its manifest: {s}"))?;
    let (provider, server) = endpoints::<Provider>().map_err(|e| format!("{e:?}"))?;
    let handle = |role, id, handle| StartupHandle { role, id, handle };
    let request = SpawnRequest {
        path: service.binary.clone(),
        arguments: arguments(&service.binary).map_err(|s| format!("arguments: {s}"))?,
        handles: vec![
            handle(HandleRole::User, MANIFEST_ID, sealed),
            handle(HandleRole::Provider, 0, server.into_handle()),
        ],
        app_id: Some(format!("system.{}", service.name)),
        user: CALLER_USER,
        login_session: false,
    };
    let spawned = match launcher.spawn(request).await {
        Ok(Ok(spawned)) => spawned,
        Ok(Err(status)) => return Err(format!("Spawn failed: {status:?}")),
        Err(e) => return Err(format!("Spawn failed: {e:?}")),
    };
    let provider = ProviderClient::new(provider);
    let (client, server) = endpoints::<Broker>().map_err(|e| format!("{e:?}"))?;
    provider
        .connect(PROTOCOL.into(), None, server.into_handle(), me)
        .map_err(|e| format!("connecting: {e:?}"))?;
    let broker = BrokerAsyncClient::new(client, reactor.clone());
    let permissions = PROCESS_MANAGER_PERMISSIONS.iter();
    let granted = broker
        .grant_system(
            PROCESS_MANAGER.into(),
            permissions.map(|p| (*p).into()).collect(),
        )
        .await;
    match granted {
        Ok(Ok(_)) => {}
        Ok(Err(status)) => return Err(format!("registering {PROCESS_MANAGER}: {status:?}")),
        Err(e) => return Err(format!("registering {PROCESS_MANAGER}: {e:?}")),
    }
    Ok(Running {
        pid: spawned.pid,
        provider,
        broker,
    })
}

/// A memory object that holds `bytes`, sealed against writes, as a
/// handle without `WRITE`.
fn sealed_copy(bytes: &[u8]) -> Result<OwnedHandle, sys::Status> {
    let mem = memory(bytes)?;
    // SAFETY: the call takes no pointers.
    let status = unsafe { sys::mem_seal(mem.raw(), SEAL_WRITE) };
    if status != mikros_rt::OK {
        return Err(status);
    }
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(mem.raw(), MANIFEST_RIGHTS, &mut out) };
    if status != mikros_rt::OK {
        return Err(status);
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}

/// A new memory object of whole pages that starts with `bytes`.
fn memory(bytes: &[u8]) -> Result<OwnedHandle, sys::Status> {
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
