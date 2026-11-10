//! A program that the process manager tests start in place of the
//! service manager, from a boot image of their own, to drive the
//! permission broker. It spawns the broker with a manifest and a
//! `Provider` channel, connects to it as several callers, and exits
//! with 0 when every check passed or with the number of the first that
//! failed.
//!
//! It accepts the `Bind` requests on the control endpoint that arrives
//! as the `USER` startup handle with ID 0, for itself and its children.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
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
    ClientIdentity, ProcessDirectory, ProcessDirectoryAsyncServer, ProcessDirectoryBindResponse,
    ProcessDirectoryRequest, Provider, ProviderClient,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const CONTROL_ID: u32 = 0;
const BROKER: &str = "/usr/bin/permissions-broker";
const PROTOCOL: &str = "mikros.permissions.Broker";
const CALLER_USER: u32 = u32::MAX;
const FOREVER: i64 = i64::MAX;
const PAGE_SIZE: u64 = 4096;
const SEAL_WRITE: u32 = 1 << 3;
/// Rights of the manifest the broker is given: no `WRITE`.
const READ_ONLY: u32 = 1 | 1 << 1 | 1 << 2 | 1 << 5 | 1 << 9;
const SAME_RIGHTS: u32 = 1 << 31;

const MANIFEST: &str = "\
name: permissions
binary: /usr/bin/permissions-broker
start: boot
provides:
  - protocol: mikros.permissions.Broker
permissions: [logs.read]
";

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let control_reactor = reactor.clone();
    let control_spawner = spawner.clone();
    let received = executor.block_on(managed::receive_async(
        bootstrap,
        reactor.clone(),
        |handles: &mut Vec<StartupHandle>| {
            let at = handles
                .iter()
                .position(|h| h.role == HandleRole::User && h.id == CONTROL_ID);
            if let Some(at) = at {
                let end = handles.remove(at).handle;
                control_spawner.spawn(serve_control(end, control_reactor.clone()));
            }
        },
    ));
    let Ok(managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let launcher = LauncherAsyncClient::new(managed.header.launcher, reactor.clone());
    executor.block_on(async move {
        match probe(&launcher, &reactor, &spawner).await {
            Ok(()) => 0,
            Err(step) => step,
        }
    })
}

/// Accepts every `Bind` request on the control endpoint `end`, and
/// keeps the bound connections open.
async fn serve_control(end: OwnedHandle, reactor: Reactor) {
    let server = ProcessDirectoryAsyncServer::new(ServerEnd::<ProcessDirectory>::new(end), reactor);
    let mut bound = Vec::new();
    while let Ok(ProcessDirectoryRequest::Bind(request, responder)) = server.next().await {
        bound.push(request.directory);
        let _ = responder.reply(server.server(), Ok(ProcessDirectoryBindResponse {}));
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
        app_id: Some("system.permissions".into()),
        user: CALLER_USER,
        login_session: false,
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
    ClientIdentity {
        pid: 0,
        user,
        app_id: app_id.into(),
    }
}

/// A connection to the broker on `provider`, for `caller`.
fn connect(provider: &ProviderClient, caller: &str, reactor: &Reactor) -> BrokerAsyncClient {
    let (client, server) = endpoints::<Broker>().expect("a channel");
    let _ = provider.connect(
        PROTOCOL.into(),
        None,
        server.into_handle(),
        identity(0, caller),
    );
    BrokerAsyncClient::new(client, reactor.clone())
}

async fn check(b: &BrokerAsyncClient, user: u32, app: &str, p: &str) -> Result<Decision, Status> {
    match b.check(identity(user, app), p.into()).await {
        Ok(r) => r.map(|r| r.decision),
        Err(_) => Err(Status::PeerClosed),
    }
}

async fn request(b: &BrokerAsyncClient, app: &str, p: &str) -> Result<Decision, Status> {
    match b
        .request(identity(1, app), p.into(), "testing".into())
        .await
    {
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

/// Runs every check in turn, and returns the number of the first that
/// failed.
async fn probe(l: &LauncherAsyncClient, reactor: &Reactor, spawner: &Spawner) -> Result<(), i64> {
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
        check(&manager, 1, APP, "network").await == Ok(Decision::Allowed),
    )?;
    c(
        "checking an undecided user permission",
        check(&manager, 1, APP, "camera").await == Ok(Decision::Undetermined),
    )?;
    c(
        "checking an unknown permission",
        check(&manager, 1, APP, "nope").await == Err(Status::NotFound),
    )?;
    c(
        "granting the broker its manifest's permissions",
        check(&manager, 0, "system.permissions", "logs.read").await == Ok(Decision::Allowed),
    )?;
    c(
        "refusing a caller without permissions.check",
        check(&stranger, 1, APP, "network").await == Err(Status::AccessDenied),
    )?;
    c(
        "denying a request without a prompter",
        request(&manager, APP, "camera").await == Ok(Decision::Denied),
    )?;
    c(
        "recording nothing without a prompter",
        check(&manager, 1, APP, "camera").await == Ok(Decision::Undetermined),
    )?;
    c(
        "refusing to grant a user permission as a system one",
        grant_system(&manager, ADMIN, &["camera"]).await == Err(Status::InvalidArgs),
    )?;
    let admin_permissions = [
        "permissions.check",
        "permissions.manage",
        "permissions.prompt",
    ];
    c(
        "granting system permissions",
        grant_system(&manager, ADMIN, &admin_permissions).await == Ok(()),
    )?;

    let admin = connect(&provider, ADMIN, reactor);
    let watcher = connect(&provider, ADMIN, reactor);
    c(
        "refusing to set a system permission",
        set(&admin, "time.set", Decision::Allowed).await == Err(Status::InvalidArgs),
    )?;
    c(
        "setting a grant",
        set(&admin, "camera", Decision::Allowed).await == Ok(()),
    )?;
    c(
        "checking the grant",
        check(&admin, 1, APP, "camera").await == Ok(Decision::Allowed),
    )?;
    let listed = admin.list_grants(1).await;
    c(
        "listing the grant",
        listed.is_ok_and(|r| r.is_ok_and(|r| r.grants.len() == 1)),
    )?;
    let watched = watcher.watch("camera".into()).await;
    c("watching", watched.is_ok_and(|r| r.is_ok()))?;
    c(
        "revoking the grant",
        set(&admin, "camera", Decision::Denied).await == Ok(()),
    )?;
    let event = watcher.next_event().await;
    c(
        "telling the watcher",
        matches!(event, Ok(BrokerEvent::OnRevoked(e))
            if e.user == 1 && e.app_id == APP && e.permission == "camera"),
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
        request(&admin, APP, "microphone").await == Ok(Decision::Allowed),
    )?;
    c(
        "recording its answer",
        check(&admin, 1, APP, "microphone").await == Ok(Decision::Allowed),
    )?;
    c(
        "not asking for a decided permission",
        request(&admin, APP, "camera").await == Ok(Decision::Denied),
    )
}
