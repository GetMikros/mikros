//! Starting the permission broker, before every other service, and
//! registering permissions with it.

use crate::boot::BootHandles;
use crate::launch::{self, Started};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use mikros_manifest::Service;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient};
use mikros_rt::interfaces::mikros::process::{HandleRole, LauncherAsyncClient, StartupHandle};
use mikros_rt::interfaces::mikros::service::ClientIdentity;
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
/// The `id` of the manifest among the broker's `USER` startup handles.
const MANIFEST_ID: u32 = 0;
const SEAL_WRITE: u32 = 1 << 3;
/// The rights the broker's manifest is given: those of a new memory
/// object without `WRITE`.
const MANIFEST_RIGHTS: u32 = DUPLICATE | TRANSFER | READ | MAP | INSPECT;
const DUPLICATE: u32 = 1 << 0;
const TRANSFER: u32 = 1 << 1;
const READ: u32 = 1 << 2;
const MAP: u32 = 1 << 5;
const INSPECT: u32 = 1 << 9;

/// Starts the broker of `service`, whose manifest is `manifest`,
/// connects to it as the service manager `me`, and registers the
/// process manager's permissions. Returns the started broker and the
/// connection.
pub async fn start(
    launcher: &LauncherAsyncClient,
    boot: &BootHandles,
    reactor: &Reactor,
    me: ClientIdentity,
    service: &Service,
    manifest: &[u8],
) -> Result<(Started, Rc<BrokerAsyncClient>), String> {
    let sealed = sealed_copy(manifest).map_err(|s| format!("sealing its manifest: {s}"))?;
    let handles = vec![StartupHandle {
        role: HandleRole::User,
        id: MANIFEST_ID,
        handle: sealed,
    }];
    let started = launch::launch(launcher, boot, service, handles).await?;
    let (client, server) = endpoints::<Broker>().map_err(|e| format!("{e:?}"))?;
    started
        .provider
        .connect(PROTOCOL.into(), None, server.into_handle(), me)
        .map_err(|e| format!("connecting: {e:?}"))?;
    let broker = Rc::new(BrokerAsyncClient::new(client, reactor.clone()));
    let permissions = PROCESS_MANAGER_PERMISSIONS.iter().map(|p| (*p).into());
    grant(&broker, PROCESS_MANAGER, permissions.collect()).await?;
    Ok((started, broker))
}

/// Registers `permissions` for `app_id` with the broker.
pub async fn grant(
    broker: &BrokerAsyncClient,
    app_id: &str,
    permissions: Vec<String>,
) -> Result<(), String> {
    match broker.grant_system(app_id.into(), permissions).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(status)) => Err(format!("registering {app_id}: {status:?}")),
        Err(e) => Err(format!("registering {app_id}: {e:?}")),
    }
}

/// A memory object that holds `bytes`, sealed against writes, as a
/// handle without `WRITE`.
fn sealed_copy(bytes: &[u8]) -> Result<OwnedHandle, sys::Status> {
    let mem = launch::memory(bytes)?;
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
