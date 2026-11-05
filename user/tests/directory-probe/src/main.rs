//! A program that the service manager starts as a boot service, to
//! drive its `Directory` endpoint from a service's side. It connects
//! through its directory to the permission broker, to protocols that do
//! not exist or that it may not use, and to a protocol it provides
//! itself. It then registers named instances through the registrar,
//! lists, watches and connects to them, and removes them. It exits with
//! 0 when every check passed or with the number of the first that
//! failed. Started as `system.restart-probe`, it exits with 1 at once,
//! for the service manager to restart it. Started as
//! `system.demand-probe`, an on-demand service, it greets the one
//! connection it is started for and exits; as `system.demand-dep`, the
//! service that the demand probe lists in `after`, it only runs. The
//! probe connects to the demand probe twice, so that it starts twice.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, DirectoryAsyncClient, DirectoryEvent, Provider, ProviderAsyncServer,
    ProviderRequest, Registrar, RegistrarAsyncClient,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const PROVIDER_ID: u32 = 0;
const BROKER: &str = "mikros.permissions.Broker";
const MISSING: &str = "test.directory.None";
const OPEN: &str = "test.directory.Open";
const PROTECTED: &str = "test.directory.Protected";
const REGISTRAR: &str = "mikros.service.Registrar";
const NAMED: &str = "test.directory.Named";
const INFO_BASIC: u32 = 1;
const PEER_CLOSED: u32 = 1 << 2;
const INFO_BASIC_SIZE: usize = 24;
/// The app ID of the program when it runs as the restart probe, and as
/// the on-demand demand probe and its dependency.
const RESTART_PROBE: &str = "system.restart-probe";
const DEMAND_PROBE: &str = "system.demand-probe";
const DEMAND_DEP: &str = "system.demand-dep";
const DEMAND: &str = "test.directory.Demand";
/// What the demand probe writes on each connection.
const HELLO: &[u8] = b"hi";
/// The step a failure to set the checks up ends with.
const SETUP: i64 = 100;
/// The step that wrong boot handles end with.
const BOOT_HANDLES: i64 = 101;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(
        bootstrap,
        reactor.clone(),
        |_: &mut Vec<StartupHandle>| {},
    ));
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    match managed.header.app_id.as_str() {
        // The restart probe fails at once, every time.
        RESTART_PROBE => return 1,
        // The dependency of the demand probe only runs.
        DEMAND_DEP => executor.block_on(core::future::pending::<()>()),
        DEMAND_PROBE => return demanded(&mut managed.handles, &mut executor, &reactor),
        _ => {}
    }
    // The boot handles its manifest lists: the log, which it logs
    // through, and the platform information, without a framebuffer.
    if !boot_handles(&managed.handles) {
        return BOOT_HANDLES;
    }
    let at = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::Provider && h.id == PROVIDER_ID);
    let Some(at) = at else {
        return SETUP;
    };
    let provider = ServerEnd::<Provider>::new(managed.handles.remove(at).handle);
    let provider = ProviderAsyncServer::new(provider, reactor.clone());
    let header = managed.header;
    let own = ClientIdentity {
        pid: header.pid,
        user: header.user,
        app_id: header.app_id,
    };
    let directory = DirectoryAsyncClient::new(header.directory, reactor.clone());
    executor.block_on(async move {
        match probe(&directory, &provider, &own, &reactor).await {
            Ok(()) => 0,
            Err(step) => step,
        }
    })
}

/// Whether `handles` hold the log as `USER` 2, with `MANAGE`, and the
/// platform information as `USER` 5, readable without `WRITE`, and no
/// other `USER` handle. Logs through the log when they do.
fn boot_handles(handles: &[StartupHandle]) -> bool {
    const LOG: u32 = 10;
    const MEMORY_OBJECT: u32 = 3;
    const READ: u32 = 1 << 2;
    const WRITE: u32 = 1 << 3;
    const MANAGE: u32 = 1 << 8;
    let user = |id| {
        handles
            .iter()
            .find(|h| h.role == HandleRole::User && h.id == id)
            .map(|h| &h.handle)
    };
    let users = handles
        .iter()
        .filter(|h| h.role == HandleRole::User)
        .count();
    let (Some(log), Some(info)) = (user(2), user(5)) else {
        return false;
    };
    let log_ok = mikros_rt::midl::handle_info(log.raw())
        .is_some_and(|(kind, rights)| kind == LOG && rights & MANAGE != 0);
    let info_ok = mikros_rt::midl::handle_info(info.raw()).is_some_and(|(kind, rights)| {
        kind == MEMORY_OBJECT && rights & READ != 0 && rights & WRITE == 0
    });
    let mut bytes = [0u8; 8];
    // SAFETY: the pointer describes `bytes`.
    let read = unsafe { sys::mem_read(info.raw(), 0, bytes.as_mut_ptr(), 8) };
    if log_ok {
        mikros_rt::log::set_handle(log.raw());
    }
    users == 2 && log_ok && info_ok && read == mikros_rt::OK && &bytes == b"platform"
}

/// Runs as the demand probe: writes `HELLO` on the first connection
/// that its Provider channel forwards, waits for the client to close it,
/// and exits with 0, or with 1 when that fails.
fn demanded(handles: &mut Vec<StartupHandle>, executor: &mut Executor, reactor: &Reactor) -> i64 {
    let at = handles
        .iter()
        .position(|h| h.role == HandleRole::Provider && h.id == PROVIDER_ID);
    let Some(at) = at else {
        return SETUP;
    };
    let provider = ServerEnd::<Provider>::new(handles.remove(at).handle);
    let provider = ProviderAsyncServer::new(provider, reactor.clone());
    executor.block_on(async {
        let Ok(ProviderRequest::Connect(c)) = provider.next().await else {
            return 1;
        };
        // SAFETY: the pointer and length describe `HELLO`.
        let wrote = unsafe {
            sys::channel_write(
                c.server.raw(),
                HELLO.as_ptr(),
                HELLO.len() as u64,
                core::ptr::null(),
                0,
            )
        };
        if wrote != mikros_rt::OK {
            return 1;
        }
        let _ = reactor.signals(c.server.raw(), PEER_CLOSED).await;
        0
    })
}

/// Connects to `DEMAND`, which starts the demand probe, and returns
/// whether the connection carried `HELLO`. A connection made while the
/// probe from an earlier one is still ending closes without it; that is
/// tried again.
async fn hello(d: &DirectoryAsyncClient, reactor: &Reactor) -> bool {
    for _ in 0..50 {
        let Ok(client) = connect(d, DEMAND, None).await else {
            return false;
        };
        let client = client.into_handle();
        if reactor.readable(client.raw()).await.is_err() {
            return false;
        }
        let mut bytes = [0u8; 8];
        let (mut len, mut count) = (0, 0);
        // SAFETY: the pointers describe `bytes`, `len` and `count`.
        let read = unsafe {
            sys::channel_read(
                client.raw(),
                bytes.as_mut_ptr(),
                bytes.len() as u64,
                core::ptr::null_mut(),
                0,
                &mut len,
                &mut count,
            )
        };
        if read == mikros_rt::OK {
            return &bytes[..len as usize] == HELLO;
        }
        let _ = reactor.sleep_until(now() + 20_000_000).await;
    }
    false
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// The koid of the object `handle` refers to and the koid of its peer.
fn koids(handle: &OwnedHandle) -> Option<(u64, u64)> {
    let mut record = [0u8; INFO_BASIC_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let s = unsafe {
        sys::object_get_info(
            handle.raw(),
            INFO_BASIC,
            record.as_mut_ptr(),
            INFO_BASIC_SIZE as u64,
            &mut len,
        )
    };
    if s != mikros_rt::OK || len < INFO_BASIC_SIZE as u64 {
        return None;
    }
    let at = |i: usize| u64::from_le_bytes(record[i..i + 8].try_into().expect("8 bytes"));
    Some((at(0), at(8)))
}

/// Connects to `protocol`, `instance`, through `directory`, passing the
/// server end of a new channel, and returns the client end.
async fn connect(
    directory: &DirectoryAsyncClient,
    protocol: &str,
    instance: Option<&str>,
) -> Result<ClientEnd<Broker>, Status> {
    let (client, server) = endpoints::<Broker>().map_err(|_| Status::NoMemory)?;
    let instance = instance.map(String::from);
    match directory
        .connect(protocol.into(), instance, server.into_handle())
        .await
    {
        Ok(r) => r.map(|_| client),
        Err(_) => Err(Status::PeerClosed),
    }
}

async fn list(directory: &DirectoryAsyncClient, protocol: &str) -> Result<Vec<String>, Status> {
    match directory.list(protocol.into()).await {
        Ok(r) => r.map(|r| r.instances),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// Registers `instance` of `protocol` with `permission`, and returns
/// the server end of its provider.
async fn register(
    registrar: &RegistrarAsyncClient,
    protocol: &str,
    instance: &str,
    permission: Option<&str>,
) -> Result<ServerEnd<Provider>, Status> {
    let (client, server) = endpoints::<Provider>().map_err(|_| Status::NoMemory)?;
    let permission = permission.map(String::from);
    match registrar
        .register(protocol.into(), instance.into(), permission, client)
        .await
    {
        Ok(r) => r.map(|_| server),
        Err(_) => Err(Status::PeerClosed),
    }
}

async fn unregister(registrar: &RegistrarAsyncClient, instance: &str) -> Result<(), Status> {
    match registrar.unregister(NAMED.into(), instance.into()).await {
        Ok(r) => r.map(drop),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// Whether the next event of `directory` says that `instance` of
/// `NAMED` was added, or removed.
async fn told(directory: &DirectoryAsyncClient, instance: &str, added: bool) -> bool {
    match directory.next_event().await {
        Ok(DirectoryEvent::OnInstanceAdded(e)) => {
            added && e.protocol == NAMED && e.instance == instance
        }
        Ok(DirectoryEvent::OnInstanceRemoved(e)) => {
            !added && e.protocol == NAMED && e.instance == instance
        }
        Err(_) => false,
    }
}

async fn watch(directory: &DirectoryAsyncClient, protocol: &str) -> Result<(), Status> {
    match directory.watch(protocol.into()).await {
        Ok(r) => r.map(drop),
        Err(_) => Err(Status::PeerClosed),
    }
}

fn identity(user: u32, app_id: &str) -> ClientIdentity {
    ClientIdentity {
        pid: 0,
        user,
        app_id: app_id.into(),
    }
}

/// Runs every check in turn, and returns the number of the first that
/// failed. `own` is the identity the process manager recorded for this
/// program.
async fn probe(
    d: &DirectoryAsyncClient,
    provider: &ProviderAsyncServer,
    own: &ClientIdentity,
    reactor: &Reactor,
) -> Result<(), i64> {
    let mut step = 0;
    let mut check = |what: &str, passed: bool| {
        step += 1;
        if !passed {
            mikros_rt::log!(Error, "directory probe: {what} failed");
        }
        passed.then_some(()).ok_or(step)
    };

    let broker = connect(d, BROKER, None).await;
    let decision = match broker {
        Ok(client) => {
            let broker = BrokerAsyncClient::new(client, reactor.clone());
            match broker
                .check(identity(1, "test.app"), "network".into())
                .await
            {
                Ok(Ok(r)) => Some(r.decision),
                _ => None,
            }
        }
        Err(_) => None,
    };
    check(
        "checking a permission through the broker",
        decision == Some(Decision::Allowed),
    )?;
    check(
        "refusing a missing protocol",
        connect(d, MISSING, None).await.err() == Some(Status::NotFound),
    )?;
    check(
        "refusing an instance of a single-instance protocol",
        connect(d, BROKER, Some("x")).await.err() == Some(Status::InvalidArgs),
    )?;
    check(
        "refusing to list a protocol",
        list(d, BROKER).await == Err(Status::InvalidArgs)
            && list(d, MISSING).await == Err(Status::NotFound),
    )?;
    check(
        "refusing to watch a protocol",
        watch(d, BROKER).await == Err(Status::InvalidArgs),
    )?;
    check(
        "refusing a protocol without its permission",
        connect(d, PROTECTED, None).await.err() == Some(Status::AccessDenied),
    )?;

    let open = connect(d, OPEN, None).await;
    let forwarded = match &open {
        Ok(_) => provider.next().await.ok(),
        Err(_) => None,
    };
    let kept = open.ok().map(ClientEnd::into_handle);
    check(
        "forwarding a connection to its provider",
        match (forwarded, kept.as_ref().and_then(koids)) {
            (Some(ProviderRequest::Connect(r)), Some((kept, peer))) => {
                r.protocol == OPEN
                    && r.instance.is_none()
                    && r.client == *own
                    && koids(&r.server) == Some((peer, kept))
            }
            _ => false,
        },
    )?;

    let (client, server) = endpoints::<Registrar>().map_err(|_| SETUP)?;
    let connected = d
        .connect(REGISTRAR.into(), None, server.into_handle())
        .await;
    check(
        "connecting to the registrar",
        matches!(connected, Ok(Ok(_))),
    )?;
    let r = RegistrarAsyncClient::new(client, reactor.clone());
    let one = register(&r, NAMED, "one", None).await;
    check("registering an instance", one.is_ok())?;
    let one = ProviderAsyncServer::new(one.map_err(|_| SETUP)?, reactor.clone());
    check(
        "refusing an instance twice",
        register(&r, NAMED, "one", None).await.err() == Some(Status::AlreadyExists),
    )?;
    check(
        "refusing an instance of a provided protocol",
        register(&r, OPEN, "x", None).await.err() == Some(Status::AlreadyExists),
    )?;
    check(
        "refusing an unknown permission",
        register(&r, NAMED, "bad", Some("nope")).await.err() == Some(Status::InvalidArgs),
    )?;
    let watched = d.watch(NAMED.into()).await;
    check("watching named instances", matches!(watched, Ok(Ok(_))))?;
    let two = register(&r, NAMED, "two", None).await;
    check(
        "telling of an added instance",
        two.is_ok() && told(d, "two", true).await,
    )?;
    let secret = register(&r, NAMED, "secret", Some("camera")).await;
    check("registering a protected instance", secret.is_ok())?;
    let listed = list(d, NAMED).await;
    check(
        "listing only the instances it may use",
        listed.as_deref() == Ok(&[String::from("one"), String::from("two")][..]),
    )?;
    check(
        "refusing a named protocol without an instance",
        connect(d, NAMED, None).await.err() == Some(Status::InvalidArgs),
    )?;
    check(
        "refusing a missing instance",
        connect(d, NAMED, Some("zzz")).await.err() == Some(Status::NotFound),
    )?;
    check(
        "refusing a protected instance",
        connect(d, NAMED, Some("secret")).await.err() == Some(Status::AccessDenied),
    )?;
    let named = connect(d, NAMED, Some("one")).await;
    let forwarded = match &named {
        Ok(_) => one.next().await.ok(),
        Err(_) => None,
    };
    check(
        "forwarding a connection to an instance",
        matches!(forwarded, Some(ProviderRequest::Connect(c))
            if c.protocol == NAMED && c.instance.as_deref() == Some("one") && c.client == *own),
    )?;
    check(
        "unregistering an instance",
        unregister(&r, "two").await == Ok(()) && told(d, "two", false).await,
    )?;
    check(
        "refusing a missing instance to unregister",
        unregister(&r, "two").await == Err(Status::NotFound),
    )?;
    drop(one);
    check(
        "removing an instance whose provider closed",
        told(d, "one", false).await && list(d, NAMED).await == Ok(Vec::new()),
    )?;
    check(
        "starting an on-demand service at the first connection",
        hello(d, reactor).await,
    )?;
    check("starting it again once it ended", hello(d, reactor).await)
}
