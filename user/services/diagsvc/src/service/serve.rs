//! Serving `mikros.diag.Overview` in the diagnostic build.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::diag::{
    Overview, OverviewAsyncServer, OverviewRequest, OverviewSnapshotResponse,
};
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::process::{HandleRole, LauncherAsyncClient};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, DirectoryAsyncClient, ProviderAsyncServer, ProviderRequest,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

const OVERVIEW: &str = "mikros.diag.Overview";
const BROKER: &str = "mikros.permissions.Broker";
const DIAG_READ: &str = "diag.read";
/// The `id` of the `root_resource` boot handle among the `USER` startup
/// handles.
const ROOT_RESOURCE_ID: u32 = 1;
const KIND_DIAG: u32 = 9;
const ERR_BUFFER_TOO_SMALL: sys::Status = -15;
/// The most bytes of text `Snapshot` returns.
const TEXT_MAX: usize = 60000;
/// The size of the first buffer the kernel's account is read into.
const FIRST_BUFFER: usize = 64 << 10;

/// What serving needs: the `DIAG` resource, the broker and the process
/// manager.
struct Service {
    diag: OwnedHandle,
    broker: Option<BrokerAsyncClient>,
    launcher: LauncherAsyncClient,
}

pub fn run(bootstrap: sys::Handle) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    // The service has no log handle: its messages go to the log service.
    mikros_rt::log::connect(&managed.header.directory);
    let mut take = |role: HandleRole, id: Option<u32>| {
        let at = managed
            .handles
            .iter()
            .position(|h| h.role == role && id.is_none_or(|id| h.id == id))?;
        Some(managed.handles.remove(at).handle)
    };
    let Some(provider) = take(HandleRole::Provider, None) else {
        return PANIC_EXIT_CODE;
    };
    let Some(root) = take(HandleRole::User, Some(ROOT_RESOURCE_ID)) else {
        mikros_rt::log!(Error, "diag: no root resource");
        return PANIC_EXIT_CODE;
    };
    let mut diag = 0;
    // SAFETY: `diag` is room for the new handle.
    let status = unsafe { sys::resource_create(root.raw(), KIND_DIAG, 0, 0, 0, &mut diag) };
    // The service keeps no more than the DIAG resource.
    drop(root);
    if status != mikros_rt::OK {
        mikros_rt::log!(Error, "diag: creating the DIAG resource failed: {status}");
        return PANIC_EXIT_CODE;
    }
    // SAFETY: the call made the handle, which nothing else owns.
    let diag = unsafe { OwnedHandle::from_raw(diag) };
    let directory = DirectoryAsyncClient::new(managed.header.directory, reactor.clone());
    let broker = executor.block_on(async {
        let (client, server) = endpoints::<Broker>().ok()?;
        let connected = directory
            .connect(BROKER.into(), None, server.into_handle())
            .await;
        matches!(connected, Ok(Ok(_))).then(|| BrokerAsyncClient::new(client, reactor.clone()))
    });
    let launcher = LauncherAsyncClient::new(managed.header.launcher, reactor.clone());
    let service = Rc::new(Service {
        diag,
        broker,
        launcher,
    });
    let provider = ProviderAsyncServer::new(ServerEnd::new(provider), reactor.clone());
    executor.block_on(async {
        while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
            // Other protocols and named instances are not ours; dropping
            // the server end closes the connection.
            if c.instance.is_none() && c.protocol == OVERVIEW {
                let end = ServerEnd::new(c.server);
                spawner.spawn(serve(service.clone(), end, reactor.clone(), c.client));
            }
        }
    });
    PANIC_EXIT_CODE
}

/// Serves the overview on `end` for `client` until it closes the
/// connection.
async fn serve(
    service: Rc<Service>,
    end: ServerEnd<Overview>,
    reactor: Reactor,
    client: ClientIdentity,
) {
    let server = OverviewAsyncServer::new(end, reactor);
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            OverviewRequest::Snapshot(_, responder) => {
                let result = match service.may_read(&client).await {
                    true => {
                        (service.snapshot().await).map(|text| OverviewSnapshotResponse { text })
                    }
                    false => Err(Status::AccessDenied),
                };
                responder.reply(s, result)
            }
        };
    }
}

impl Service {
    /// Whether `client` holds `diag.read`, as the broker says.
    async fn may_read(&self, client: &ClientIdentity) -> bool {
        let Some(broker) = &self.broker else {
            return false;
        };
        let client = ClientIdentity {
            pid: client.pid,
            user: client.user,
            app_id: client.app_id.clone(),
        };
        let checked = broker.check(client, DIAG_READ.into()).await;
        matches!(checked, Ok(Ok(r)) if r.decision == Decision::Allowed)
    }

    /// The kernel's account of its state as text tables.
    async fn snapshot(&self) -> Result<String, Status> {
        let identities = self.identities().await;
        let raw = self.read()?;
        let text = core::str::from_utf8(&raw).map_err(|_| Status::Internal)?;
        let identity = |koid| {
            identities
                .get(&koid)
                .map(|(user, app_id)| crate::snapshot::Identity {
                    user: *user,
                    app_id: app_id.clone(),
                })
        };
        Ok(crate::snapshot::render(text, &identity, TEXT_MAX))
    }

    /// The user and app ID of each process the process manager knows, by
    /// its koid. A process it cannot tell of is left out.
    async fn identities(&self) -> BTreeMap<u64, (u32, String)> {
        let mut identities = BTreeMap::new();
        let Ok(listed) = self.launcher.list().await else {
            return identities;
        };
        for pid in listed.pids {
            if let Ok(Ok(got)) = self.launcher.get_info(pid).await {
                identities.insert(got.info.koid, (got.info.user, got.info.app_id));
            }
        }
        identities
    }

    /// The kernel's account, read into a buffer that grows to the size the
    /// kernel says it needs.
    fn read(&self) -> Result<Vec<u8>, Status> {
        let mut size = FIRST_BUFFER;
        loop {
            let mut buffer = Vec::new();
            buffer
                .try_reserve_exact(size)
                .map_err(|_| Status::NoMemory)?;
            buffer.resize(size, 0);
            let mut actual = 0u64;
            // SAFETY: the buffer holds `size` bytes, and `actual` is room
            // for the size.
            let status = unsafe {
                sys::diag_snapshot(
                    self.diag.raw(),
                    buffer.as_mut_ptr(),
                    size as u64,
                    &mut actual,
                )
            };
            match status {
                mikros_rt::OK => {
                    buffer.truncate(actual as usize);
                    return Ok(buffer);
                }
                // The account grew meanwhile; some room is left for more.
                ERR_BUFFER_TOO_SMALL => size = actual as usize + actual as usize / 4,
                _ => {
                    mikros_rt::log!(Error, "diag: diag_snapshot failed: {status}");
                    return Err(Status::Internal);
                }
            }
        }
    }
}
