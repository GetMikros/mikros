//! The permission broker.
//!
//! It takes its manifest from the `USER` startup handle with ID 0, which
//! must be a read-only, write-sealed memory object, and starts from the
//! trusted grants of `crate::permissions::Store::bootstrap`. Without a
//! valid manifest it ends. It then serves `mikros.permissions.Broker` on
//! each connection that the service manager forwards on its `PROVIDER`
//! startup handle, to the caller that the connection names. It asks the
//! session manager for `admin` permissions through its own directory.

mod broker;
mod files;
use crate::permissions::Store;
use alloc::vec;
use alloc::vec::Vec;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::process::{HandleRole, LauncherAsyncClient, StartupHandle};
use mikros_rt::interfaces::mikros::service::{
    DirectoryAsyncClient, ProviderAsyncServer, ProviderRequest,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, objects, sys};

mikros_rt::raw_main!(main);

/// The folder of the manifest, which names the service `permbroker`.
const FOLDER: &str = "system.permbroker";
/// The app ID the service manager starts the broker with.
const APP_ID: &str = "system.permbroker";
/// The protocol the broker provides.
const PROTOCOL: &str = "mikros.permissions.Broker";
/// The numbers of app IDs, for every process.
const APP_NUMBERS: &str = "mikros.permissions.AppNumbers";
/// The `id` of the manifest among the `USER` startup handles.
const MANIFEST_ID: u32 = 0;

const INFO_MEMORY: u32 = 4;
const INFO_MEMORY_SIZE: usize = 0x18;
const SEAL_WRITE: u32 = 1 << 3;
const RIGHT_WRITE: u32 = 1 << 3;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let Some(store) = take(&mut managed.handles, HandleRole::User, MANIFEST_ID)
        .and_then(|mem| manifest_store(&mem))
    else {
        return PANIC_EXIT_CODE;
    };
    let Some(provider) = take(&mut managed.handles, HandleRole::Provider, 0) else {
        return PANIC_EXIT_CODE;
    };
    let directory = DirectoryAsyncClient::new(managed.header.directory, reactor.clone());
    let launcher = LauncherAsyncClient::new(managed.header.launcher, reactor.clone());
    let broker = crate::service::broker::Broker::new(
        store,
        directory,
        launcher,
        reactor.clone(),
        spawner.clone(),
    );
    let provider = ProviderAsyncServer::new(ServerEnd::new(provider), reactor);
    executor.block_on(async {
        while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
            // Other protocols and named instances are not ours; dropping
            // the server end closes the connection.
            if c.protocol == PROTOCOL && c.instance.is_none() {
                broker.serve(ServerEnd::new(c.server), c.client);
            } else if c.protocol == APP_NUMBERS && c.instance.is_none() {
                broker.serve_numbers(ServerEnd::new(c.server), c.client);
            }
        }
    });
    PANIC_EXIT_CODE
}

/// Removes the startup handle with `role` and `id` from `handles`.
fn take(handles: &mut Vec<StartupHandle>, role: HandleRole, id: u32) -> Option<OwnedHandle> {
    let at = handles.iter().position(|h| h.role == role && h.id == id)?;
    Some(handles.remove(at).handle)
}

/// The store that the manifest in `mem` starts the broker with, or
/// `None` when `mem` is writable, not write-sealed, or not a valid
/// manifest of this service.
fn manifest_store(mem: &OwnedHandle) -> Option<Store> {
    let (_, rights) = mikros_rt::midl::handle_info(mem.raw())?;
    if rights & RIGHT_WRITE != 0 {
        return None;
    }
    let mut record = [0u8; INFO_MEMORY_SIZE];
    let len = objects::info(mem.raw(), INFO_MEMORY, &mut record).ok()?;
    if len < INFO_MEMORY_SIZE {
        return None;
    }
    let size = u64::from_le_bytes(record[..8].try_into().ok()?);
    let seals = u32::from_le_bytes(record[16..20].try_into().ok()?);
    if seals & SEAL_WRITE == 0 || size > mikros_yaml::SIZE_MAX as u64 {
        return None;
    }
    let mut bytes = vec![0u8; size as usize];
    objects::read_memory(mem.raw(), 0, &mut bytes).ok()?;
    // The object may be larger than the manifest, which then ends at
    // the zero bytes that fill the rest.
    let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let manifest = mikros_manifest::parse(FOLDER, &bytes[..end]).ok()?;
    Store::bootstrap(APP_ID, &manifest.service.permissions).ok()
}
