//! A program that the process manager tests start in place of the
//! service manager, from a boot image of their own, to drive the
//! process manager's `Launcher` endpoint. It also runs as the children
//! it spawns, chosen by its arguments:
//!
//! - none: drives `Spawn`, `Wait` and `Kill`, and exits with 0 when
//!   every check passed or with the number of the first that failed;
//! - `exit <n>`: exits with `n`;
//! - `sleep`: waits until it is killed;
//! - `rename`: exits with 0 when it may not name an app ID, 1 otherwise;
//! - `exec`: replaces itself with `SELF_PATH` run with `sleep`.
//!
//! It also spawns `/usr/bin/hello`, a static C program, and
//! `/usr/bin/hello-std`, a Rust program that uses the standard library,
//! with a stream as their standard output, and checks what they print.
//!
//! It accepts the `Bind` requests on the control endpoint that arrives
//! as the `USER` startup handle with ID 0, for itself and its children,
//! and serves their directories, which provide only a clock with a time
//! page.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Spawner;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherAsyncClient, ProcessInfo, SpawnRequest, StartupHandle, StatusKind,
    WaitOptions,
};
use mikros_rt::interfaces::mikros::service::{
    Directory, DirectoryAsyncServer, DirectoryConnectResponse, DirectoryRequest, ProcessDirectory,
    ProcessDirectoryAsyncServer, ProcessDirectoryBindResponse, ProcessDirectoryRequest,
};
use mikros_rt::interfaces::mikros::time::{
    Clock, ClockAsyncServer, ClockGetTimePageResponse, ClockRequest,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const CONTROL_ID: u32 = 0;
const PROBE: &str = "/usr/bin/probe";
/// The probe's own path, as init starts it in place of the service
/// manager; the image holds the probe there too.
const SELF_PATH: &str = "/usr/bin/service-manager";
/// A static C program, which prints a greeting for its argument.
const HELLO: &str = "/usr/bin/hello";
/// The same program linked with the shared C library.
const HELLO_DYNAMIC: &str = "/usr/bin/hello-dynamic";
/// A C program that needs a shared library.
const LIBRARY_USER: &str = "/usr/bin/library-user";
/// A Rust program that uses the standard library, which opens
/// `/dev/null` for its closed standard input and error.
const HELLO_STD: &str = "/usr/bin/hello-std";
/// A Rust program that starts processes with `std::process::Command`.
const SPAWN_STD: &str = "/usr/bin/spawn-std";
/// The `id` of a stream as descriptor 1.
const STDOUT_STREAM: u32 = 1 | 2 << 28;
const KILL: u32 = 9;
const CALLER_USER: u32 = u32::MAX;
const FOREVER: i64 = i64::MAX;
const PAGE_SIZE: u64 = 4096;
/// UTC minus monotonic time on the time page the probe hands out: 1
/// January 2024.
const UTC_OFFSET: i64 = 1_704_067_200 * 1_000_000_000;
const TRANSFER: u32 = 1 << 1;
const READ: u32 = 1 << 2;
const MAP: u32 = 1 << 5;
const INSPECT: u32 = 1 << 9;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let control_reactor = reactor.clone();
    let received = executor.block_on(managed::receive_async(
        bootstrap,
        reactor.clone(),
        |handles: &mut Vec<StartupHandle>| {
            let at = handles
                .iter()
                .position(|h| h.role == HandleRole::User && h.id == CONTROL_ID);
            if let Some(at) = at {
                let end = handles.remove(at).handle;
                let tasks = spawner.clone();
                spawner.spawn(serve_control(end, control_reactor.clone(), tasks));
            }
        },
    ));
    let Ok(managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let args = arguments(&managed.header.arguments);
    let me = managed.header.pid;
    let launcher = LauncherAsyncClient::new(managed.header.launcher, reactor.clone());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    executor.block_on(async move {
        match args.as_slice() {
            [_] => probe(&launcher, &reactor, me).await,
            [_, "exit", code] => code.parse().unwrap_or(PANIC_EXIT_CODE),
            [_, "sleep"] => core::future::pending().await,
            [_, "rename"] => {
                let named = spawn(&launcher, &["exit", "0"], |r| {
                    r.app_id = Some("system.renamed".into())
                })
                .await;
                i64::from(named != Err(Status::AccessDenied))
            }
            [_, "exec"] => {
                let request = SpawnRequest {
                    path: SELF_PATH.into(),
                    arguments: arguments_from(&[SELF_PATH, "sleep"]),
                    handles: Vec::new(),
                    app_id: None,
                    user: CALLER_USER,
                    login_session: false,
                };
                // On success this process is killed before the reply.
                let _ = launcher.exec(request).await;
                PANIC_EXIT_CODE
            }
            _ => PANIC_EXIT_CODE,
        }
    })
}

/// Accepts every `Bind` request on the control endpoint `end`, and
/// serves each bound directory.
async fn serve_control(end: OwnedHandle, reactor: Reactor, spawner: Spawner) {
    let server =
        ProcessDirectoryAsyncServer::new(ServerEnd::<ProcessDirectory>::new(end), reactor.clone());
    while let Ok(ProcessDirectoryRequest::Bind(request, responder)) = server.next().await {
        let tasks = spawner.clone();
        spawner.spawn(serve_directory(request.directory, reactor.clone(), tasks));
        let _ = responder.reply(server.server(), Ok(ProcessDirectoryBindResponse {}));
    }
}

/// Serves a child's directory: the first connection to
/// `mikros.time.Clock` finds no service, as before the time service
/// starts, and later ones reach a clock that hands out a time page with
/// `UTC_OFFSET`. No other protocol is provided.
async fn serve_directory(end: ServerEnd<Directory>, reactor: Reactor, spawner: Spawner) {
    let server = DirectoryAsyncServer::new(end, reactor.clone());
    let mut clock_refused = false;
    while let Ok(request) = server.next().await {
        let DirectoryRequest::Connect(q, r) = request else {
            continue;
        };
        let clock = q.protocol == "mikros.time.Clock";
        let result = if clock && core::mem::replace(&mut clock_refused, true) {
            let clock = ServerEnd::<Clock>::new(q.server);
            spawner.spawn(serve_clock(clock, reactor.clone()));
            Ok(DirectoryConnectResponse {})
        } else {
            Err(Status::NotFound)
        };
        let _ = r.reply(server.server(), result);
    }
}

async fn serve_clock(end: ServerEnd<Clock>, reactor: Reactor) {
    let server = ClockAsyncServer::new(end, reactor);
    while let Ok(request) = server.next().await {
        if let ClockRequest::GetTimePage(_, r) = request {
            let _ = r.reply(
                server.server(),
                ClockGetTimePageResponse { page: time_page() },
            );
        }
    }
}

/// A read-only time page whose offset is `UTC_OFFSET`.
fn time_page() -> OwnedHandle {
    let mut page = [0u8; 16];
    page[4..8].copy_from_slice(&1u32.to_le_bytes());
    page[8..16].copy_from_slice(&UTC_OFFSET.to_le_bytes());
    let (mut mem, mut read_only) = (0, 0);
    // SAFETY: the pointers describe `mem`, `page` and `read_only`.
    unsafe {
        sys::mem_create(PAGE_SIZE, 0, &mut mem);
        sys::mem_write(mem, 0, page.as_ptr(), page.len() as u64);
        sys::handle_duplicate(mem, TRANSFER | READ | MAP | INSPECT, &mut read_only);
        sys::handle_close(mem);
        OwnedHandle::from_raw(read_only)
    }
}

/// The arguments in the `arguments` memory object.
fn arguments(mem: &OwnedHandle) -> Vec<String> {
    let mut bytes = alloc::vec![0u8; PAGE_SIZE as usize];
    // SAFETY: the pointer describes `bytes`.
    let status = unsafe { sys::mem_read(mem.raw(), 0, bytes.as_mut_ptr(), PAGE_SIZE) };
    if status != mikros_rt::OK {
        return Vec::new();
    }
    let count = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    bytes[8..]
        .split(|&b| b == 0)
        .take(count)
        .map(|s| String::from_utf8_lossy(s).into())
        .collect()
}

/// An `arguments` memory object with `PROBE` and then `args`.
fn arguments_of(args: &[&str]) -> OwnedHandle {
    let all: Vec<&str> = core::iter::once(PROBE)
        .chain(args.iter().copied())
        .collect();
    arguments_from(&all)
}

/// An `arguments` memory object with `args`.
fn arguments_from(args: &[&str]) -> OwnedHandle {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(args.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for arg in args {
        bytes.extend_from_slice(arg.as_bytes());
        bytes.push(0);
    }
    let mut mem = 0;
    // SAFETY: the pointers describe `mem` and `bytes`.
    unsafe {
        sys::mem_create(PAGE_SIZE, 0, &mut mem);
        sys::mem_write(mem, 0, bytes.as_ptr(), bytes.len() as u64);
        OwnedHandle::from_raw(mem)
    }
}

/// Runs the program at `path` with the argument `world` and a stream as
/// its standard output, and returns what it printed and its exit code.
/// Its standard input and error are left closed.
async fn run_c(l: &LauncherAsyncClient, path: &str) -> (Vec<u8>, Option<i64>) {
    let (output, stdout) = stream();
    let pid = spawn(l, &[], |r| {
        r.path = path.into();
        r.arguments = arguments_from(&[path, "world"]);
        r.handles.push(StartupHandle {
            role: HandleRole::Stdout,
            id: STDOUT_STREAM,
            handle: stdout,
        });
    })
    .await
    .map_or(0, |(pid, _)| pid);
    let ended = wait(l, pid as i32).await;
    let code = ended.and_then(|(p, code, signal)| (p == pid && signal == 0).then_some(code));
    if code != Some(0) {
        mikros_rt::log!(Error, "launcher probe: {path} ended with {ended:?}");
    }
    (read_all(&output), code)
}

/// A new stream: the end the probe reads, and the end it gives away.
fn stream() -> (OwnedHandle, OwnedHandle) {
    let (mut a, mut b) = (0, 0);
    // SAFETY: the out pointers are valid for the call, and the handles
    // were just created for us.
    unsafe {
        sys::stream_create(0, &mut a, &mut b);
        (OwnedHandle::from_raw(a), OwnedHandle::from_raw(b))
    }
}

/// Every byte queued toward `stream`.
fn read_all(stream: &OwnedHandle) -> Vec<u8> {
    let mut bytes = alloc::vec![0u8; PAGE_SIZE as usize];
    let mut actual = 0;
    // SAFETY: the pointers describe `bytes` and `actual`.
    let status =
        unsafe { sys::stream_read(stream.raw(), bytes.as_mut_ptr(), PAGE_SIZE, &mut actual) };
    bytes.truncate(if status == mikros_rt::OK {
        actual as usize
    } else {
        0
    });
    bytes
}

/// Spawns `PROBE` with `args`, after `change` adjusted the request.
async fn spawn(
    launcher: &LauncherAsyncClient,
    args: &[&str],
    change: impl FnOnce(&mut SpawnRequest),
) -> Result<(u32, OwnedHandle), Status> {
    let mut request = SpawnRequest {
        path: PROBE.into(),
        arguments: arguments_of(args),
        handles: Vec::new(),
        app_id: None,
        user: CALLER_USER,
        login_session: false,
    };
    change(&mut request);
    match launcher.spawn(request).await {
        Ok(Ok(r)) => Ok((r.pid, r.process)),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// Waits for the child `selector` chooses, returning its ID, exit code
/// and terminating signal.
async fn wait(launcher: &LauncherAsyncClient, selector: i32) -> Option<(u32, i64, u32)> {
    let r = launcher
        .wait(selector, WaitOptions(0), FOREVER)
        .await
        .ok()?
        .ok()?;
    (r.status.kind == StatusKind::Exited).then_some((
        r.pid,
        r.status.exit_code,
        r.status.term_signal,
    ))
}

async fn get_info(launcher: &LauncherAsyncClient, pid: u32) -> Result<ProcessInfo, Status> {
    match launcher.get_info(pid).await {
        Ok(result) => result.map(|r| r.info),
        Err(_) => Err(Status::PeerClosed),
    }
}

async fn kill(launcher: &LauncherAsyncClient, pid: u32, signo: u32) -> Result<(), Status> {
    match launcher.kill(pid, signo).await {
        Ok(result) => result.map(drop),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// Runs every check in turn as process `me` on the executor of
/// `reactor`, and returns the number of the first that failed, or 0.
async fn probe(l: &LauncherAsyncClient, reactor: &Reactor, me: u32) -> i64 {
    let mut step = 0;
    let mut check = |what: &str, passed: bool| {
        step += 1;
        if !passed {
            mikros_rt::log!(Error, "launcher probe: {what} failed");
        }
        passed.then_some(()).ok_or(step)
    };
    let checks = async {
        let exits = spawn(l, &["exit", "7"], |_| {}).await;
        check("spawning a child as pid 3", matches!(&exits, Ok((3, _))))?;
        check(
            "waiting for its exit code",
            wait(l, 3).await == Some((3, 7, 0)),
        )?;
        let sleeper = spawn(l, &["sleep"], |_| {}).await.map(|(pid, _)| pid);
        check("spawning a sleeping child", sleeper.is_ok())?;
        let sleeper = sleeper.unwrap_or(0);
        check("killing it", kill(l, sleeper, KILL).await.is_ok())?;
        check(
            "seeing KILL end it",
            wait(l, -1).await == Some((sleeper, -3, KILL)),
        )?;
        check(
            "refusing to signal init",
            kill(l, 1, 0).await == Err(Status::AccessDenied),
        )?;
        check(
            "refusing to signal an unknown process",
            kill(l, 1000, 0).await == Err(Status::NotFound),
        )?;
        let refused = async |path: &str| spawn(l, &[], |r| r.path = path.into()).await.err();
        check(
            "refusing a relative path",
            refused("usr/bin/probe").await == Some(Status::InvalidArgs),
        )?;
        check(
            "refusing a missing program",
            refused("/usr/bin/none").await == Some(Status::NotFound),
        )?;
        check(
            "refusing a directory",
            refused("/usr/bin").await == Some(Status::AccessDenied),
        )?;
        let other = |change: fn(&mut SpawnRequest)| spawn(l, &["exit", "0"], change);
        check(
            "refusing another user",
            other(|r| r.user = 5).await.err() == Some(Status::AccessDenied),
        )?;
        check(
            "refusing a login session",
            other(|r| r.login_session = true).await.err() == Some(Status::AccessDenied),
        )?;
        check(
            "refusing an app ID outside system",
            other(|r| r.app_id = Some("test.other".into())).await.err()
                == Some(Status::AccessDenied),
        )?;
        let named = spawn(l, &["exit", "4"], |r| {
            r.app_id = Some("system.named".into())
        })
        .await;
        check("naming a system app ID", named.is_ok())?;
        let named = named.map_or(0, |(pid, _)| pid);
        check(
            "waiting for it",
            wait(l, named as i32).await == Some((named, 4, 0)),
        )?;
        let rename = spawn(l, &["rename"], |_| {})
            .await
            .map_or(0, |(pid, _)| pid);
        check(
            "refusing a child an app ID",
            wait(l, rename as i32).await == Some((rename, 0, 0)),
        )?;
        let own = get_info(l, me).await;
        check(
            "describing itself",
            own.as_ref()
                .is_ok_and(|i| i.pid == me && i.parent == 1 && i.path == SELF_PATH),
        )?;
        let sleeper = spawn(l, &["sleep"], |_| {}).await.map_or(0, |(pid, _)| pid);
        let info = get_info(l, sleeper).await;
        check(
            "describing a child",
            info.as_ref().is_ok_and(|i| {
                i.pid == sleeper
                    && i.parent == me
                    && i.path == PROBE
                    && own
                        .as_ref()
                        .is_ok_and(|o| o.user == i.user && o.app_id == i.app_id)
            }),
        )?;
        check("ending it", kill(l, sleeper, KILL).await.is_ok())?;
        check("waiting for it", wait(l, sleeper as i32).await.is_some())?;
        check(
            "finding no process 999",
            get_info(l, 999).await.err() == Some(Status::NotFound),
        )?;
        // A process that replaced itself reports its new program.
        let replaced = spawn(l, &["exec"], |_| {}).await.map_or(0, |(pid, _)| pid);
        let mut path = String::new();
        for _ in 0..1000 {
            path = get_info(l, replaced)
                .await
                .map(|i| i.path)
                .unwrap_or_default();
            if path == SELF_PATH {
                break;
            }
            let mut now = 0;
            // SAFETY: the pointer is valid for the call.
            unsafe { sys::clock_get_monotonic(&mut now) };
            let _ = reactor.sleep_until(now + 1_000_000).await;
        }
        check("describing a replaced process", path == SELF_PATH)?;
        check("ending it", kill(l, replaced, KILL).await.is_ok())?;
        check("waiting for it", wait(l, replaced as i32).await.is_some())?;
        let (output, code) = run_c(l, HELLO).await;
        check("running a static C program", code == Some(0))?;
        check(
            "reading what it printed",
            output.as_slice() == b"hello, world\n",
        )?;
        let (output, code) = run_c(l, HELLO_DYNAMIC).await;
        check("running a dynamically linked C program", code == Some(0))?;
        check(
            "reading what it printed",
            output.as_slice() == b"hello, world\n",
        )?;
        let (output, code) = run_c(l, LIBRARY_USER).await;
        check("running a C program with a shared library", code == Some(0))?;
        check(
            "reading what the library wrote",
            output.as_slice() == b"hello from a library, call 1\n",
        )?;
        let (output, code) = run_c(l, HELLO_STD).await;
        check("running a Rust program that uses std", code == Some(0))?;
        check(
            "reading what it printed",
            output.as_slice() == b"hello, world, from std\n",
        )?;
        let (output, code) = run_c(l, SPAWN_STD).await;
        check("starting processes with std's Command", code == Some(0))?;
        check(
            "reading what it printed",
            output.as_slice() == b"spawned, world\n",
        )?;
        Ok(())
    };
    match checks.await {
        Ok(()) => 0,
        Err(step) => step,
    }
}
