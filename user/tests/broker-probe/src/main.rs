//! A program that the process manager tests start in place of the
//! service manager, from a boot image of their own, to drive the
//! permission broker. It spawns the broker with a manifest and a
//! `Provider` channel, connects to it as several callers, and exits
//! with 0 when every check passed or with the number of the first that
//! failed.
//!
//! It accepts the `Bind` requests on the control endpoint that arrives
//! as the `USER` startup handle with ID 0, for itself and its children,
//! and serves on each bound directory a stub of the session manager's
//! `Authorization`, which allows only `test.admin` and counts how often
//! it was asked.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::permissions::{
    Broker, BrokerAsyncClient, BrokerEvent, Decision, Grant, Prompter, PrompterAskResponse,
    PrompterAsyncServer, PrompterRequest,
};
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherAsyncClient, SpawnRequest, StartupHandle, StatusKind, WaitOptions,
};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, Directory, DirectoryAsyncServer, DirectoryConnectResponse, DirectoryRequest,
    ProcessDirectory, ProcessDirectoryAsyncServer, ProcessDirectoryBindResponse,
    ProcessDirectoryRequest, Provider, ProviderClient,
};
use mikros_rt::interfaces::mikros::session::{
    Authorization, AuthorizationAsyncServer, AuthorizationAuthorizeResponse, AuthorizationRequest,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const CONTROL_ID: u32 = 0;
const BROKER: &str = "/usr/bin/permbroker";
const PROTOCOL: &str = "mikros.permissions.Broker";
const CALLER_USER: u32 = u32::MAX;
const FOREVER: i64 = i64::MAX;
const PAGE_SIZE: u64 = 4096;
const SEAL_WRITE: u32 = 1 << 3;
/// Rights of the manifest the broker is given: no `WRITE`.
const READ_ONLY: u32 = 1 | 1 << 1 | 1 << 2 | 1 << 5 | 1 << 9;
const SAME_RIGHTS: u32 = 1 << 31;
const AUTHORIZATION: &str = "mikros.session.Authorization";
/// The kernel object ID that the probe's connections to the broker
/// name as their process's.
const KOID: u64 = 77;

const MANIFEST: &str = "\
name: permbroker
binary: /usr/bin/permbroker
start: boot
provides:
  - protocol: mikros.permissions.Broker
permissions: [admin.logs.read]
";

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let control_reactor = reactor.clone();
    let control_spawner = spawner.clone();
    let asked = Rc::new(Cell::new(0));
    let stub_asked = asked.clone();
    let received = executor.block_on(managed::receive_async(
        bootstrap,
        reactor.clone(),
        |handles: &mut Vec<StartupHandle>| {
            let at = handles
                .iter()
                .position(|h| h.role == HandleRole::User && h.id == CONTROL_ID);
            if let Some(at) = at {
                let end = handles.remove(at).handle;
                let (reactor, spawner) = (control_reactor.clone(), control_spawner.clone());
                control_spawner.spawn(serve_control(end, reactor, spawner, stub_asked.clone()));
            }
        },
    ));
    let Ok(managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let launcher = LauncherAsyncClient::new(managed.header.launcher, reactor.clone());
    executor.block_on(async move {
        match probe(&launcher, &reactor, &spawner, &asked).await {
            Ok(()) => 0,
            Err(step) => step,
        }
    })
}

/// Accepts every `Bind` request on the control endpoint `end`, and
/// serves each bound directory, counting the asks of its stubs in
/// `asked`.
async fn serve_control(end: OwnedHandle, reactor: Reactor, spawner: Spawner, asked: Rc<Cell<u32>>) {
    let control = ServerEnd::<ProcessDirectory>::new(end);
    let server = ProcessDirectoryAsyncServer::new(control, reactor.clone());
    while let Ok(ProcessDirectoryRequest::Bind(request, responder)) = server.next().await {
        let directory = ServerEnd::new(request.directory.into_handle());
        let serving = serve_directory(directory, reactor.clone(), spawner.clone(), asked.clone());
        spawner.spawn(serving);
        let _ = responder.reply(server.server(), Ok(ProcessDirectoryBindResponse {}));
    }
}

/// Serves `Directory` on `end`: `Authorization` as the stub, and no
/// other protocol.
async fn serve_directory(
    end: ServerEnd<Directory>,
    reactor: Reactor,
    spawner: Spawner,
    asked: Rc<Cell<u32>>,
) {
    let server = DirectoryAsyncServer::new(end, reactor.clone());
    while let Ok(request) = server.next().await {
        let DirectoryRequest::Connect(q, r) = request else {
            continue;
        };
        if q.protocol != AUTHORIZATION {
            let _ = r.reply(server.server(), Err(Status::NotFound));
            continue;
        }
        let stub = ServerEnd::<Authorization>::new(q.server);
        spawner.spawn(authorization(stub, reactor.clone(), asked.clone()));
        let _ = r.reply(server.server(), Ok(DirectoryConnectResponse {}));
    }
}

/// The stub of `Authorization`: allows `test.admin` only, as user 1000.
async fn authorization(end: ServerEnd<Authorization>, reactor: Reactor, asked: Rc<Cell<u32>>) {
    let server = AuthorizationAsyncServer::new(end, reactor);
    while let Ok(AuthorizationRequest::Authorize(q, r)) = server.next().await {
        asked.set(asked.get() + 1);
        let allowed = q.client.app_id == ADMIN;
        let admin = if allowed { 1000 } else { 0 };
        let reply = AuthorizationAuthorizeResponse { allowed, admin };
        let _ = r.reply(server.server(), Ok(reply));
    }
}

fn memory(bytes: &[u8]) -> OwnedHandle {
    let mut mem = 0;
    // SAFETY: the pointers describe `mem` and `bytes`.
    unsafe {
        sys::mem_create(PAGE_SIZE, 0, &mut mem);
        sys::mem_write(mem, 0, bytes.as_ptr(), bytes.len() as u64);
        OwnedHandle::from_raw(mem)
    }
}

/// The manifest as a memory object, sealed against writes when `seal`,
/// with `rights`.
fn manifest(seal: bool, rights: u32) -> OwnedHandle {
    let mem = memory(MANIFEST.as_bytes());
    let mut out = 0;
    // SAFETY: the calls take the out pointer only.
    unsafe {
        if seal {
            sys::mem_seal(mem.raw(), SEAL_WRITE);
        }
        sys::handle_duplicate(mem.raw(), rights, &mut out);
        OwnedHandle::from_raw(out)
    }
}

/// An `arguments` memory object that holds only `BROKER`.
fn arguments() -> OwnedHandle {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(BROKER.as_bytes());
    bytes.push(0);
    memory(&bytes)
}

/// Spawns the broker with the manifest `manifest`, and returns its pid
/// and the client of its `Provider` channel.
async fn spawn_broker(
    launcher: &LauncherAsyncClient,
    manifest: OwnedHandle,
) -> Option<(u32, ProviderClient)> {
    let (client, server) = endpoints::<Provider>().ok()?;
    let handle = |role, handle| StartupHandle {
        role,
        id: 0,
        handle,
    };
    let request = SpawnRequest {
        path: BROKER.into(),
        arguments: arguments(),
        handles: vec![
            handle(HandleRole::User, manifest),
            handle(HandleRole::Provider, server.into_handle()),
        ],
        app_id: Some("system.permbroker".into()),
        user: CALLER_USER,
        login_session: false,
        new_session: false,
        working_directory: None,
        umask: u32::MAX,
    };
    let spawned = launcher.spawn(request).await.ok()?.ok()?;
    Some((spawned.pid, ProviderClient::new(client)))
}

/// The exit code of child `pid`, once it ends.
async fn exit_code(launcher: &LauncherAsyncClient, pid: u32) -> Option<i64> {
    let r = launcher
        .wait(pid as i32, WaitOptions(0), FOREVER)
        .await
        .ok()?
        .ok()?;
    (r.status.kind == StatusKind::Exited).then_some(r.status.exit_code)
}

fn identity(user: u32, app_id: &str) -> ClientIdentity {
    process(user, app_id, 0)
}

/// The identity of a process of `user` and `app_id` with `koid`.
fn process(user: u32, app_id: &str, koid: u64) -> ClientIdentity {
    ClientIdentity {
        pid: 0,
        user,
        app_id: app_id.into(),
        admin: 0,
        koid,
    }
}

/// A connection to the broker on `provider`, for `caller`.
fn connect(provider: &ProviderClient, caller: &str, reactor: &Reactor) -> BrokerAsyncClient {
    connect_as(provider, caller, KOID, reactor)
}

/// A connection to the broker on `provider`, for `caller` in the process
/// with `koid`.
fn connect_as(
    provider: &ProviderClient,
    caller: &str,
    koid: u64,
    reactor: &Reactor,
) -> BrokerAsyncClient {
    let (client, server) = endpoints::<Broker>().expect("a channel");
    let _ = provider.connect(
        PROTOCOL.into(),
        None,
        server.into_handle(),
        process(0, caller, koid),
    );
    BrokerAsyncClient::new(client, reactor.clone())
}

async fn check(b: &BrokerAsyncClient, user: u32, app: &str, p: &str) -> Result<Decision, Status> {
    match b.check(identity(user, app), p.into()).await {
        Ok(r) => r.map(|r| r.decision),
        Err(_) => Err(Status::PeerClosed),
    }
}

async fn check_for(
    b: &BrokerAsyncClient,
    client: ClientIdentity,
    p: &str,
) -> Result<Decision, Status> {
    match b.check(client, p.into()).await {
        Ok(r) => r.map(|r| r.decision),
        Err(_) => Err(Status::PeerClosed),
    }
}

async fn request(b: &BrokerAsyncClient, app: &str, p: &str) -> Result<Decision, Status> {
    request_for(b, identity(1, app), p).await
}

async fn request_for(
    b: &BrokerAsyncClient,
    client: ClientIdentity,
    p: &str,
) -> Result<Decision, Status> {
    match b.request(client, p.into(), "testing".into()).await {
        Ok(r) => r.map(|r| r.decision),
        Err(_) => Err(Status::PeerClosed),
    }
}

async fn set(b: &BrokerAsyncClient, p: &str, decision: Decision) -> Result<(), Status> {
    let grant = Grant {
        user: 1,
        app_id: APP.into(),
        permission: p.into(),
        decision,
    };
    match b.set_grant(grant).await {
        Ok(r) => r.map(drop),
        Err(_) => Err(Status::PeerClosed),
    }
}

async fn grant_system(b: &BrokerAsyncClient, app: &str, p: &[&str]) -> Result<(), Status> {
    let p = p.iter().map(|p| String::from(*p)).collect();
    match b.grant_system(app.into(), p).await {
        Ok(r) => r.map(drop),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// Serves `end`, answering every `Ask` with `allowed`.
async fn prompter(end: ServerEnd<Prompter>, reactor: Reactor, allowed: bool) {
    let server = PrompterAsyncServer::new(end, reactor);
    while let Ok(PrompterRequest::Ask(_, r)) = server.next().await {
        let _ = r.reply(server.server(), PrompterAskResponse { allowed });
    }
}

const MANAGER: &str = "system.servicemgr";
const ADMIN: &str = "test.admin";
const APP: &str = "test.app";
/// An app that may manage grants but no administrator allows.
const MANAGER_APP: &str = "test.manager";

/// Runs every check in turn, and returns the number of the first that
/// failed.
async fn probe(
    l: &LauncherAsyncClient,
    reactor: &Reactor,
    spawner: &Spawner,
    asked: &Cell<u32>,
) -> Result<(), i64> {
    let mut step = 0;
    let mut check_ = |what: &str, passed: bool| {
        step += 1;
        if !passed {
            mikros_rt::log!(Error, "broker probe: {what} failed");
        }
        passed.then_some(()).ok_or(step)
    };
    let c = &mut check_;

    // The broker ends without a read-only, write-sealed manifest.
    for (seal, rights) in [(false, READ_ONLY), (true, SAME_RIGHTS)] {
        let spawned = spawn_broker(l, manifest(seal, rights)).await;
        c("spawning a broker with a bad manifest", spawned.is_some())?;
        let (pid, _provider) = spawned.ok_or(100)?;
        c(
            "ending without a good manifest",
            exit_code(l, pid).await == Some(PANIC_EXIT_CODE),
        )?;
    }

    let spawned = spawn_broker(l, manifest(true, READ_ONLY)).await;
    c("spawning the broker", spawned.is_some())?;
    let (_, provider) = spawned.ok_or(100)?;
    let manager = connect(&provider, MANAGER, reactor);
    let stranger = connect(&provider, APP, reactor);

    c(
        "checking a default permission",
        check(&manager, 1, APP, "default.network.use").await == Ok(Decision::Allowed),
    )?;
    c(
        "checking an undecided user permission",
        check(&manager, 1, APP, "user.camera.use").await == Ok(Decision::Undetermined),
    )?;
    c(
        "checking an unknown permission",
        check(&manager, 1, APP, "nope").await == Err(Status::NotFound),
    )?;
    c(
        "granting the broker its manifest's permissions",
        check(&manager, 0, "system.permbroker", "admin.logs.read").await == Ok(Decision::Allowed),
    )?;
    c(
        "refusing a caller without system.permissions.read",
        check(&stranger, 1, APP, "default.network.use").await == Err(Status::AccessDenied),
    )?;
    c(
        "denying a request without a prompter",
        request(&manager, APP, "user.camera.use").await == Ok(Decision::Denied),
    )?;
    c(
        "recording nothing without a prompter",
        check(&manager, 1, APP, "user.camera.use").await == Ok(Decision::Undetermined),
    )?;
    c(
        "refusing to grant a user permission as a system one",
        grant_system(&manager, ADMIN, &["user.camera.use"]).await == Err(Status::InvalidArgs),
    )?;
    let admin_permissions = [
        "system.permissions.read",
        "system.grants.manage",
        "system.prompts.use",
    ];
    c(
        "granting system permissions",
        grant_system(&manager, ADMIN, &admin_permissions).await == Ok(()),
    )?;

    let admin = connect(&provider, ADMIN, reactor);
    let watcher = connect(&provider, ADMIN, reactor);
    c(
        "refusing to set a system permission",
        set(&admin, "system.devices.manage", Decision::Allowed).await == Err(Status::InvalidArgs),
    )?;
    c(
        "refusing to set an admin permission",
        set(&admin, "admin.time.set", Decision::Denied).await == Err(Status::InvalidArgs),
    )?;
    c("refusing them before asking", asked.get() == 0)?;
    c(
        "granting a manager of grants",
        grant_system(&manager, MANAGER_APP, &["system.grants.manage"]).await == Ok(()),
    )?;
    let other = connect_as(&provider, MANAGER_APP, KOID + 1, reactor);
    c(
        "refusing another user's grant that no administrator allows",
        set(&other, "user.camera.use", Decision::Denied).await == Err(Status::AccessDenied),
    )?;
    c("asking the administrator", asked.get() == 1)?;
    c(
        "setting a grant that an administrator allows",
        set(&admin, "user.camera.use", Decision::Allowed).await == Ok(()),
    )?;
    c("asking the administrator again", asked.get() == 2)?;
    c(
        "checking the grant",
        check(&admin, 1, APP, "user.camera.use").await == Ok(Decision::Allowed),
    )?;
    let listed = admin.list_grants(1).await;
    c(
        "listing the grant",
        listed.is_ok_and(|r| r.is_ok_and(|r| r.grants.len() == 1)),
    )?;
    c("holding the administrator's answer", asked.get() == 2)?;
    c(
        "denying an admin permission to no process",
        request(&manager, ADMIN, "admin.time.set").await == Ok(Decision::Denied),
    )?;
    c("not asking for no process", asked.get() == 2)?;
    c(
        "allowing an admin permission that an administrator allows",
        request_for(&manager, process(1, ADMIN, 5), "admin.time.set").await
            == Ok(Decision::Allowed),
    )?;
    c(
        "holding it for that process",
        check_for(&manager, process(1, ADMIN, 5), "admin.time.set").await == Ok(Decision::Allowed),
    )?;
    c(
        "holding it for no other process",
        check_for(&manager, process(1, ADMIN, 6), "admin.time.set").await
            == Ok(Decision::Undetermined),
    )?;
    c(
        "denying an admin permission that no administrator allows",
        request_for(&manager, process(1, APP, 8), "admin.time.set").await == Ok(Decision::Denied),
    )?;
    c("asking twice more", asked.get() == 4)?;
    let watched = watcher.watch("user.camera.use".into()).await;
    c("watching", watched.is_ok_and(|r| r.is_ok()))?;
    c(
        "revoking the grant",
        set(&admin, "user.camera.use", Decision::Denied).await == Ok(()),
    )?;
    let event = watcher.next_event().await;
    c(
        "telling the watcher",
        matches!(event, Ok(BrokerEvent::OnRevoked(e))
            if e.user == 1 && e.app_id == APP && e.permission == "user.camera.use"),
    )?;

    let (client, server) = endpoints::<Prompter>().map_err(|_| 100)?;
    spawner.spawn(prompter(server, reactor.clone(), true));
    let registered = admin.register_prompter(client).await;
    c(
        "registering a prompter",
        registered.is_ok_and(|r| r.is_ok()),
    )?;
    c(
        "asking the prompter",
        request(&admin, APP, "user.microphone.use").await == Ok(Decision::Allowed),
    )?;
    c(
        "recording its answer",
        check(&admin, 1, APP, "user.microphone.use").await == Ok(Decision::Allowed),
    )?;
    c(
        "not asking for a decided permission",
        request(&admin, APP, "user.camera.use").await == Ok(Decision::Denied),
    )
}
