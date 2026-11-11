//! A program that runs the test suites of ported projects. The ported
//! software test starts it in place of the service manager, from a boot
//! image of its own that holds the tests, what they read, and the plan
//! `PLAN`.
//!
//! The plan lists the tests to run, each as lines of a keyword, a space
//! and a value, ending with a line `end`:
//!
//! - `test <name>`: the name of the test, which begins it;
//! - `path <path>`: the program to run;
//! - `arg <argument>`: the next argument, the first being the program's
//!   name;
//! - `env <name>=<value>`: a variable of its environment;
//! - `timeout <seconds>`: how long it may run before it is killed;
//! - `expect fail`: the test is expected to fail, not to pass.
//!
//! Each test runs in turn, with one stream as its standard output and
//! error, and passes when it exits with 0. The runner logs every result,
//! and the end of the output of each test that unexpectedly failed, to
//! the `USER` startup handle with ID 2, and exits with the number of
//! tests whose result was not the expected one.
//!
//! It accepts the `Bind` requests on the control endpoint that arrives
//! as the `USER` startup handle with ID 0, and serves its children's
//! directories, which provide no protocols.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::Poll;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{Node, NodeAsyncClient, OpenFlags, Rights};
use mikros_rt::interfaces::mikros::process::{
    ExitStatus, HandleRole, LauncherAsyncClient, SpawnRequest, StartupHandle, StatusKind,
    WaitOptions,
};
use mikros_rt::interfaces::mikros::service::{
    Directory, DirectoryAsyncServer, DirectoryRequest, ProcessDirectory,
    ProcessDirectoryAsyncServer, ProcessDirectoryBindResponse, ProcessDirectoryRequest,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const CONTROL_ID: u32 = 0;
const LOG_ID: u32 = 2;
/// The plan, in the boot image.
const PLAN: &str = "usr/share/port-tests/plan";
/// The IDs of the standard output and error, as stream descriptors.
const STDOUT_STREAM: u32 = 1 | 2 << 28;
const STDERR_STREAM: u32 = 2 | 2 << 28;
const KILL: u32 = 9;
const CALLER_USER: u32 = u32::MAX;
const FOREVER: i64 = i64::MAX;
const PAGE_SIZE: u64 = 4096;
const READABLE: u32 = 1 << 0;
const PEER_CLOSED: u32 = 1 << 2;
/// The most of a test's output that is kept, from its end.
const TAIL: usize = 4096;
/// The most bytes one `Read` of the plan returns.
const READ_MAX: u32 = 32768;

/// A test of the plan.
#[derive(Default)]
struct Test {
    name: String,
    path: String,
    args: Vec<String>,
    env: Vec<String>,
    timeout: i64,
    expect_fail: bool,
}

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
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let log = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::User && h.id == LOG_ID)
        .map(|at| managed.handles.swap_remove(at).handle);
    if let Some(log) = &log {
        mikros_rt::log::set_handle(log.raw());
    }
    let root = managed
        .namespace
        .iter()
        .position(|e| e.path == "/")
        .map(|at| managed.namespace.swap_remove(at).directory);
    let Some(root) = root else {
        mikros_rt::log!(Error, "port tests: the namespace has no root");
        return PANIC_EXIT_CODE;
    };
    let root = NodeAsyncClient::new(root, reactor.clone());
    let launcher = LauncherAsyncClient::new(managed.header.launcher, reactor.clone());
    let failed = executor.block_on(async move {
        let Some(plan) = read(&root, &reactor, PLAN).await else {
            mikros_rt::log!(Error, "port tests: reading the plan failed");
            return PANIC_EXIT_CODE;
        };
        let Some(tests) = parse(&plan) else {
            mikros_rt::log!(Error, "port tests: the plan is not valid");
            return PANIC_EXIT_CODE;
        };
        let mut unexpected = 0;
        for test in &tests {
            if !run(&launcher, &reactor, test).await {
                unexpected += 1;
            }
        }
        let expected = tests.len() as i64 - unexpected;
        mikros_rt::log!(
            Info,
            "port tests: {expected} of {} as expected",
            tests.len()
        );
        unexpected
    });
    drop(log);
    failed
}

/// Accepts every `Bind` request on the control endpoint `end`, and
/// serves each bound directory.
async fn serve_control(end: OwnedHandle, reactor: Reactor, spawner: Spawner) {
    let server =
        ProcessDirectoryAsyncServer::new(ServerEnd::<ProcessDirectory>::new(end), reactor.clone());
    while let Ok(ProcessDirectoryRequest::Bind(request, responder)) = server.next().await {
        spawner.spawn(serve_directory(request.directory, reactor.clone()));
        let _ = responder.reply(server.server(), Ok(ProcessDirectoryBindResponse {}));
    }
}

/// Serves a child's directory, which finds no service for any protocol.
async fn serve_directory(end: ServerEnd<Directory>, reactor: Reactor) {
    let server = DirectoryAsyncServer::new(end, reactor);
    while let Ok(request) = server.next().await {
        if let DirectoryRequest::Connect(_, r) = request {
            let _ = r.reply(server.server(), Err(Status::NotFound));
        }
    }
}

/// The contents of the file at `path` under `root`.
async fn read(root: &NodeAsyncClient, reactor: &Reactor, path: &str) -> Option<Vec<u8>> {
    let (client, server) = endpoints::<Node>().ok()?;
    root.open(path.into(), OpenFlags(0), 0, Rights::READ, server)
        .await
        .ok()?
        .ok()?;
    let file = NodeAsyncClient::new(client, reactor.clone());
    let mut bytes = Vec::new();
    loop {
        let data = file
            .read(bytes.len() as u64, READ_MAX)
            .await
            .ok()?
            .ok()?
            .data;
        if data.is_empty() {
            return Some(bytes);
        }
        bytes.extend_from_slice(&data);
    }
}

/// The tests of `plan`, or `None` when it is not valid.
fn parse(plan: &[u8]) -> Option<Vec<Test>> {
    let plan = core::str::from_utf8(plan).ok()?;
    let mut tests = Vec::new();
    let mut test: Option<Test> = None;
    for line in plan.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match (key, &mut test) {
            ("test", None) => {
                test = Some(Test {
                    name: value.into(),
                    ..Test::default()
                })
            }
            ("path", Some(t)) => t.path = value.into(),
            ("arg", Some(t)) => t.args.push(value.into()),
            ("env", Some(t)) => t.env.push(value.into()),
            ("timeout", Some(t)) => t.timeout = value.parse().ok()?,
            ("expect", Some(t)) if value == "fail" => t.expect_fail = true,
            ("end", Some(_)) => tests.push(test.take()?),
            _ => return None,
        }
    }
    test.is_none().then_some(tests)
}

/// Runs `test` and returns whether it passed or, when it is expected to,
/// failed.
async fn run(launcher: &LauncherAsyncClient, reactor: &Reactor, test: &Test) -> bool {
    let name = &test.name;
    let (output, stdout) = stream();
    let Some(stderr) = duplicate(&stdout) else {
        mikros_rt::log!(Error, "port test {name}: duplicating its stream failed");
        return false;
    };
    let request = SpawnRequest {
        path: test.path.as_str().into(),
        arguments: arguments(&test.args, &test.env),
        handles: alloc::vec![
            StartupHandle {
                role: HandleRole::Stdout,
                id: STDOUT_STREAM,
                handle: stdout,
            },
            StartupHandle {
                role: HandleRole::Stderr,
                id: STDERR_STREAM,
                handle: stderr,
            },
        ],
        app_id: None,
        user: CALLER_USER,
        login_session: false,
        new_session: false,
    };
    let pid = match launcher.spawn(request).await {
        Ok(Ok(r)) => r.pid,
        result => {
            mikros_rt::log!(Error, "port test {name}: starting it failed: {result:?}");
            return false;
        }
    };
    let deadline = now().saturating_add(test.timeout.saturating_mul(1_000_000_000));
    let ended = async {
        if let Some(status) = wait(launcher, pid, deadline).await {
            return Some((status, false));
        }
        let _ = launcher.kill(pid, KILL).await;
        wait(launcher, pid, FOREVER).await.map(|s| (s, true))
    };
    let (ended, tail) = join(ended, collect(&output, reactor)).await;
    let failure = match ended {
        Some((status, false)) if status.kind == StatusKind::Exited && status.exit_code == 0 => None,
        Some((_, true)) => Some(String::from("killed after its timeout")),
        Some((status, false)) if status.kind == StatusKind::Exited => {
            Some(alloc::format!("exited with {}", status.exit_code))
        }
        Some((status, false)) => Some(alloc::format!("ended by signal {}", status.term_signal)),
        None => Some(String::from("waiting for it failed")),
    };
    match (failure, test.expect_fail) {
        (None, false) => mikros_rt::log!(Info, "port test {name}: passed"),
        (Some(why), true) => mikros_rt::log!(Info, "port test {name}: {why}, as expected"),
        (None, true) => {
            mikros_rt::log!(Error, "port test {name}: passed, but is expected to fail");
            return false;
        }
        (Some(why), false) => {
            mikros_rt::log!(Error, "port test {name}: {why}");
            let tail = String::from_utf8_lossy(tail.as_slices().0);
            for line in tail.lines() {
                mikros_rt::log!(Error, "port test {name}| {line}");
            }
            return false;
        }
    }
    true
}

/// How the child `pid` ended, unless it did not before `deadline`.
async fn wait(launcher: &LauncherAsyncClient, pid: u32, deadline: i64) -> Option<ExitStatus> {
    let r = launcher
        .wait(pid as i32, WaitOptions(0), deadline)
        .await
        .ok()?
        .ok()?;
    (r.pid == pid).then_some(r.status)
}

/// The last `TAIL` bytes written toward `stream` until its peer closed.
async fn collect(stream: &OwnedHandle, reactor: &Reactor) -> VecDeque<u8> {
    let mut tail = VecDeque::with_capacity(TAIL);
    let mut buffer = alloc::vec![0u8; PAGE_SIZE as usize];
    loop {
        let mut actual = 0;
        // SAFETY: the pointers describe `buffer` and `actual`.
        let status =
            unsafe { sys::stream_read(stream.raw(), buffer.as_mut_ptr(), PAGE_SIZE, &mut actual) };
        if status == mikros_rt::OK {
            for &byte in &buffer[..actual as usize] {
                if tail.len() == TAIL {
                    tail.pop_front();
                }
                tail.push_back(byte);
            }
        } else {
            // Any other status means the peer closed and every byte was
            // read.
            let waiting = status == i64::from(mikros_rt::midl::SHOULD_WAIT);
            if !waiting
                || reactor
                    .signals(stream.raw(), READABLE | PEER_CLOSED)
                    .await
                    .is_err()
            {
                break;
            }
        }
    }
    tail.make_contiguous();
    tail
}

/// Runs `a` and `b` together until both completed.
async fn join<A: Future, B: Future>(a: A, b: B) -> (A::Output, B::Output) {
    let (mut a, mut b) = (pin!(a), pin!(b));
    let (mut x, mut y) = (None, None);
    poll_fn(|cx| {
        if x.is_none() {
            x = match a.as_mut().poll(cx) {
                Poll::Ready(v) => Some(v),
                Poll::Pending => None,
            };
        }
        if y.is_none() {
            y = match b.as_mut().poll(cx) {
                Poll::Ready(v) => Some(v),
                Poll::Pending => None,
            };
        }
        match (x.is_some(), y.is_some()) {
            (true, true) => Poll::Ready((x.take().unwrap(), y.take().unwrap())),
            _ => Poll::Pending,
        }
    })
    .await
}

/// An `arguments` memory object with `args` and the environment `env`.
fn arguments(args: &[String], env: &[String]) -> OwnedHandle {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(args.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(env.len() as u32).to_le_bytes());
    for string in args.iter().chain(env) {
        bytes.extend_from_slice(string.as_bytes());
        bytes.push(0);
    }
    let size = (bytes.len() as u64).next_multiple_of(PAGE_SIZE);
    let mut mem = 0;
    // SAFETY: the pointers describe `mem` and `bytes`.
    unsafe {
        sys::mem_create(size, 0, &mut mem);
        sys::mem_write(mem, 0, bytes.as_ptr(), bytes.len() as u64);
        OwnedHandle::from_raw(mem)
    }
}

/// A new stream: the end the runner reads, and the end it gives away.
fn stream() -> (OwnedHandle, OwnedHandle) {
    let (mut a, mut b) = (0, 0);
    // SAFETY: the out pointers are valid for the call, and the handles
    // were just created for us.
    unsafe {
        sys::stream_create(0, &mut a, &mut b);
        (OwnedHandle::from_raw(a), OwnedHandle::from_raw(b))
    }
}

/// Another handle to the object of `handle`, with the same rights.
fn duplicate(handle: &OwnedHandle) -> Option<OwnedHandle> {
    const SAME_RIGHTS: u32 = 1 << 31;
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle.raw(), SAME_RIGHTS, &mut out) };
    // SAFETY: the handle was just created for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}
