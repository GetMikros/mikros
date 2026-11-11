//! The boot service of `tests/term.img`, the terminal service's client.
//! It opens terminals through `Factory`, types on their master streams
//! and checks what `Terminal` reads, writes, echoes and signals, and the
//! numbers it gives the terminals. Copies of it, started with an
//! argument, are the processes that job control acts on: `sleep` waits
//! until it is killed, and `read` reads the
//! terminal whose token it gets as its `USER` startup handle 1. It exits
//! with 0 once every check passed, and logs what failed otherwise. It
//! also runs the C program `term-c` on a terminal and types what that
//! program waits for.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherClient, SpawnRequest, StartupHandle, StatusKind, WaitOptions,
};
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::interfaces::mikros::terminal::{
    Factory, FactoryClient, Terminal, TerminalClient, WindowSize,
};
use mikros_rt::{managed, sys};

mikros_rt::raw_main!(main);

const LOG_ID: u32 = 2;
const TOKEN_ID: u32 = 1;
const FACTORY: &str = "mikros.terminal.Factory";
const TERMINAL: &str = "mikros.terminal.Terminal";
const BINARY: &str = "/usr/bin/term-probe";
/// The C program run on a terminal, and the descriptor kind of a slave
/// terminal.
const C_PROGRAM: &str = "/usr/bin/term-c";
const TERMINAL_KIND: u32 = 8;
const FOREVER: i64 = i64::MAX;
const MS: i64 = 1_000_000;
const SECOND: i64 = 1000 * MS;
const CALLER_USER: u32 = u32::MAX;
const SAME_RIGHTS: u32 = 1 << 31;
const READABLE: u32 = 1 << 0;
const USER0: u32 = 1 << 24;
const USER1: u32 = 1 << 25;
const SIGINT: u32 = 2;
const SIGKILL: u32 = 9;
const SIGTTIN: u32 = 21;

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

fn failed(what: &str) -> Option<()> {
    mikros_rt::log!(Error, "term-probe: {what} failed");
    None
}

/// The reply of a call named `what`, logging why there is none.
fn reply<T, E: core::fmt::Debug, F: core::fmt::Debug>(
    what: &str,
    r: Result<Result<T, E>, F>,
) -> Option<T> {
    match r {
        Ok(Ok(t)) => Some(t),
        Ok(Err(e)) => {
            mikros_rt::log!(Error, "term-probe: {what} failed: {e:?}");
            None
        }
        Err(e) => {
            mikros_rt::log!(Error, "term-probe: {what} failed: {e:?}");
            None
        }
    }
}

fn check(what: &str, ok: bool) -> Option<()> {
    if ok { Some(()) } else { failed(what) }
}

fn connect<P>(directory: &DirectoryClient, protocol: &str) -> Option<ClientEnd<P>> {
    let (client, server) = endpoints::<P>().ok()?;
    let connected = directory.connect(protocol.into(), None, server.into_handle(), FOREVER);
    reply(protocol, connected).map(|_| client)
}

fn duplicate(handle: &OwnedHandle) -> Option<OwnedHandle> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle.raw(), SAME_RIGHTS, &mut out) };
    // SAFETY: the kernel just created the handle for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
}

/// The signals of `handle` that are set now, of `signals`.
fn signals(handle: &OwnedHandle, signals: u32, deadline: i64) -> u32 {
    let mut observed = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::object_wait_one(handle.raw(), signals, deadline, &mut observed) };
    observed & signals
}

/// Types `bytes` on the master stream.
fn type_in(master: &OwnedHandle, bytes: &[u8]) -> Option<()> {
    let mut actual = 0;
    // SAFETY: the pointers describe `bytes` and `actual`.
    let status = unsafe {
        sys::stream_write(
            master.raw(),
            bytes.as_ptr(),
            bytes.len() as u64,
            &mut actual,
        )
    };
    check(
        "typing on the master stream",
        status == mikros_rt::OK && actual as usize == bytes.len(),
    )
}

/// Reads from the master stream until it has `len` bytes, within a
/// second.
fn shown(master: &OwnedHandle, len: usize) -> Vec<u8> {
    let deadline = now() + SECOND;
    let mut got = Vec::new();
    while got.len() < len && signals(master, READABLE, deadline) != 0 {
        let mut buffer = [0u8; 256];
        let mut actual = 0;
        // SAFETY: the pointers describe `buffer` and `actual`.
        unsafe {
            sys::stream_read(
                master.raw(),
                buffer.as_mut_ptr(),
                buffer.len() as u64,
                &mut actual,
            )
        };
        got.extend_from_slice(&buffer[..actual as usize]);
    }
    got
}

/// A memory object of `arguments`: their count, no environment, and each
/// ended by a zero byte.
fn argument_memory(arguments: &[&str]) -> Option<OwnedHandle> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(arguments.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for a in arguments {
        bytes.extend_from_slice(a.as_bytes());
        bytes.push(0);
    }
    let mut mem = 0;
    let size = (bytes.len() as u64).next_multiple_of(4096);
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::mem_create(size, 0, &mut mem) } != mikros_rt::OK {
        return None;
    }
    // SAFETY: the kernel just created the handle for us.
    let mem = unsafe { OwnedHandle::from_raw(mem) };
    // SAFETY: the pointer and length describe `bytes`.
    let written = unsafe { sys::mem_write(mem.raw(), 0, bytes.as_ptr(), bytes.len() as u64) };
    (written == mikros_rt::OK).then_some(mem)
}

fn arguments(mem: &OwnedHandle) -> Vec<String> {
    let mut bytes = vec![0u8; 4096];
    // SAFETY: the pointer describes `bytes`.
    if unsafe { sys::mem_read(mem.raw(), 0, bytes.as_mut_ptr(), bytes.len() as u64) }
        != mikros_rt::OK
    {
        return Vec::new();
    }
    let count = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    bytes[8..]
        .split(|&b| b == 0)
        .take(count)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

/// Starts a copy of the probe with `mode` and `handles`, in a process
/// group of its own.
fn child(launcher: &LauncherClient, mode: &str, handles: Vec<StartupHandle>) -> Option<u32> {
    let request = SpawnRequest {
        path: BINARY.into(),
        arguments: argument_memory(&[BINARY, mode])?,
        handles,
        app_id: None,
        user: CALLER_USER,
        login_session: false,
        new_session: false,
        working_directory: None,
        umask: u32::MAX,
    };
    let pid = match launcher.spawn(request, FOREVER) {
        Ok(Ok(spawned)) => spawned.pid,
        _ => return None,
    };
    match launcher.set_process_group(pid, pid, FOREVER) {
        Ok(Ok(_)) => Some(pid),
        _ => None,
    }
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut managed) = managed::receive(bootstrap) else {
        return 1;
    };
    if let Some(log) = managed
        .handles
        .iter()
        .find(|h| h.role == HandleRole::User && h.id == LOG_ID)
    {
        mikros_rt::log::set_handle(log.handle.raw());
    }
    let header = managed.header;
    let directory = DirectoryClient::new(header.directory);
    match arguments(&header.arguments).get(1).map(String::as_str) {
        Some("sleep") => loop {
            // SAFETY: the call takes no pointers.
            unsafe { sys::thread_sleep(FOREVER) };
        },
        Some("read") => {
            let at = managed
                .handles
                .iter()
                .position(|h| h.role == HandleRole::User && h.id == TOKEN_ID);
            let (Some(at), Some(terminal)) = (at, connect::<Terminal>(&directory, TERMINAL)) else {
                return 3;
            };
            let token = managed.handles.remove(at).handle;
            let terminal = TerminalClient::new(terminal);
            // A background read stops this process before it returns.
            let read = terminal.read(token, 10, FOREVER, FOREVER);
            return if matches!(read, Ok(Err(Status::Interrupted))) {
                0
            } else {
                4
            };
        }
        _ => {}
    }
    let launcher = LauncherClient::new(header.launcher);
    match probe(&directory, &launcher) {
        Some(()) => {
            mikros_rt::log!(Info, "term-probe: PASSED");
            0
        }
        None => 2,
    }
}

fn probe(directory: &DirectoryClient, launcher: &LauncherClient) -> Option<()> {
    let factory = FactoryClient::new(connect::<Factory>(directory, FACTORY)?);
    let terminal = TerminalClient::new(connect::<Terminal>(directory, TERMINAL)?);
    let size = WindowSize {
        rows: 24,
        columns: 80,
        width: 0,
        height: 0,
    };
    let Ok(Ok(opened)) = factory.open(size, FOREVER) else {
        return failed("Open");
    };
    let (master, token) = (opened.master, opened.terminal);
    let t = || duplicate(&token).expect("a copy of the token");

    // The defaults of a new terminal.
    let a = reply("step 1", terminal.get_attributes(t(), FOREVER))?.attributes;
    check(
        "the default attributes",
        a.local_flags == 0o105073 && a.input_flags == 0o2400,
    )?;
    let size = reply("step 2", terminal.get_window_size(t(), FOREVER))?.size;
    check("the window size", size.rows == 24 && size.columns == 80)?;
    // The first terminal is number 0.
    let first = reply("the number", terminal.get_number(t(), FOREVER))?.number;
    check("the first terminal's number", first == 0)?;

    // A typed line, read as NL and echoed with CR NL.
    let readiness = reply("step 3", terminal.get_readiness(t(), FOREVER))?.event;
    check(
        "being writable",
        signals(&readiness, USER0 | USER1, 0) == USER1,
    )?;
    type_in(&master, b"hi\r")?;
    check("the echo", shown(&master, 4) == b"hi\r\n")?;
    check(
        "being readable",
        signals(&readiness, USER0, now() + SECOND) == USER0,
    )?;
    let read = reply("step 4", terminal.read(t(), 100, FOREVER, FOREVER))?.data;
    check("reading the line", read == b"hi\n")?;
    let written = terminal
        .write(t(), b"a\nb".to_vec(), FOREVER, FOREVER)
        .ok()?
        .ok()?
        .written;
    check("writing", written == 3 && shown(&master, 4) == b"a\r\nb")?;

    // Deadlines and limits.
    let r = terminal.read(t(), 10, 0, FOREVER).ok()?;
    check("a read that may not wait", r == Err(Status::ShouldWait))?;
    let r = terminal.read(t(), 10, now() + 50 * MS, FOREVER).ok()?;
    check("a read that times out", r == Err(Status::TimedOut))?;
    let r = terminal.read(t(), 40000, FOREVER, FOREVER).ok()?;
    check("refusing a long read", r == Err(Status::OutOfRange))?;
    let r = terminal.read(t(), 0, FOREVER, FOREVER).ok()?;
    check("a read of nothing", r.is_ok_and(|r| r.data.is_empty()))?;
    let mut other = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::event_create(0, &mut other) };
    // SAFETY: the kernel just created the handle for us.
    let other = unsafe { OwnedHandle::from_raw(other) };
    let r = terminal.get_attributes(other, FOREVER).ok()?;
    check(
        "refusing a token of no terminal",
        r.is_err_and(|e| e == Status::BadHandle),
    )?;

    // Job control: the probe leads a session with the terminal.
    check(
        "CreateSession",
        matches!(launcher.create_session(FOREVER), Ok(Ok(_))),
    )?;
    check(
        "MakeControlling",
        matches!(terminal.make_controlling(t(), FOREVER), Ok(Ok(_))),
    )?;
    let r = terminal.make_controlling(t(), FOREVER).ok()?;
    check(
        "refusing a second controlling terminal",
        r == Err(Status::AccessDenied),
    )?;
    let me = reply("step 5", terminal.get_foreground(t(), FOREVER))?.pgid;

    // ^C interrupts the foreground group.
    let sleeper = child(launcher, "sleep", Vec::new())?;
    let r = terminal.set_foreground(t(), sleeper, FOREVER).ok()?;
    check("SetForeground", r.is_ok())?;
    type_in(&master, b"\x03")?;
    let waited = launcher
        .wait(sleeper as i32, WaitOptions(0), now() + 5 * SECOND, FOREVER)
        .ok()?
        .ok()?;
    check(
        "interrupting the foreground group",
        waited.status.term_signal == SIGINT,
    )?;
    check(
        "SetForeground back",
        terminal.set_foreground(t(), me, FOREVER).ok()?.is_ok(),
    )?;

    // A background read stops its reader with TTIN.
    let handles = vec![StartupHandle {
        role: HandleRole::User,
        id: TOKEN_ID,
        handle: t(),
    }];
    let reader = child(launcher, "read", handles)?;
    let waited = launcher
        .wait(
            reader as i32,
            WaitOptions::STOPPED,
            now() + 5 * SECOND,
            FOREVER,
        )
        .ok()?
        .ok()?;
    let stopped = waited.status.kind == StatusKind::Stopped && waited.status.stop_signal == SIGTTIN;
    let _ = launcher.kill(reader, SIGKILL, FOREVER);
    let _ = launcher.wait(reader as i32, WaitOptions(0), now() + 5 * SECOND, FOREVER);
    check("stopping a background reader", stopped)?;

    // Input left when the master closes is read, then the end.
    let Ok(Ok(second)) = factory.open(
        WindowSize {
            rows: 1,
            columns: 1,
            width: 0,
            height: 0,
        },
        FOREVER,
    ) else {
        return failed("a second Open");
    };
    let number = terminal.get_number(duplicate(&second.terminal)?, FOREVER);
    let second_number = reply("the second number", number)?.number;
    check("the second terminal's number", second_number == 1)?;
    type_in(&second.master, b"z\n")?;
    let token2 = second.terminal;
    drop(second.master);
    let deadline = now() + SECOND;
    let r = terminal
        .read(duplicate(&token2)?, 10, deadline, FOREVER)
        .ok()?
        .ok()?
        .data;
    check("reading after the master closed", r == b"z\n")?;
    let r = terminal
        .read(duplicate(&token2)?, 10, deadline, FOREVER)
        .ok()?
        .ok()?
        .data;
    check("the end of input after the master closed", r.is_empty())?;
    // The number of a terminal whose master closed is free again.
    let Ok(Ok(third)) = factory.open(
        WindowSize {
            rows: 1,
            columns: 1,
            width: 0,
            height: 0,
        },
        FOREVER,
    ) else {
        return failed("a third Open");
    };
    let number = terminal.get_number(duplicate(&third.terminal)?, FOREVER);
    let third_number = reply("the third number", number)?.number;
    check("reusing a closed terminal's number", third_number == 1)?;
    drop(third);
    c_program(&factory, &terminal, launcher)
}

/// Runs the C program on a new terminal, as its standard input, output
/// and error, and types what it waits for.
fn c_program(
    factory: &FactoryClient,
    terminal: &TerminalClient,
    launcher: &LauncherClient,
) -> Option<()> {
    let size = WindowSize {
        rows: 24,
        columns: 80,
        width: 0,
        height: 0,
    };
    let Ok(Ok(opened)) = factory.open(size, FOREVER) else {
        return failed("the C program's Open");
    };
    let (master, token) = (opened.master, opened.terminal);
    // Descriptors 0, 1 and 2, of kind 8, a slave terminal.
    let standard = |role, fd: u32| {
        duplicate(&token).map(|handle| StartupHandle {
            role,
            id: TERMINAL_KIND << 28 | fd,
            handle,
        })
    };
    let handles = vec![
        standard(HandleRole::Stdin, 0)?,
        standard(HandleRole::Stdout, 1)?,
        standard(HandleRole::Stderr, 2)?,
    ];
    let request = SpawnRequest {
        path: C_PROGRAM.into(),
        arguments: argument_memory(&[C_PROGRAM])?,
        handles,
        app_id: None,
        user: CALLER_USER,
        login_session: false,
        new_session: false,
        working_directory: None,
        umask: u32::MAX,
    };
    let pid = reply("starting the C program", launcher.spawn(request, FOREVER))?.pid;
    // It names its terminal by the terminal's number.
    let number = terminal.get_number(duplicate(&token)?, FOREVER);
    let number = reply("the C program's terminal number", number)?.number;
    let ready = alloc::format!("ready /dev/pts/{number}\r\n");
    check(
        "the C program's first line",
        shown(&master, ready.len()) == ready.as_bytes(),
    )?;
    type_in(&master, b"line\r")?;
    check(
        "the echo of the typed line",
        shown(&master, 6) == b"line\r\n",
    )?;
    check("the C program's raw mode", shown(&master, 5) == b"raw\r\n")?;
    type_in(&master, b"x")?;
    let done = shown(&master, 6);
    let waited = launcher.wait(pid as i32, WaitOptions(0), now() + 10 * SECOND, FOREVER);
    let status = reply("waiting for the C program", waited)?.status;
    if status.kind != StatusKind::Exited || status.exit_code != 0 {
        mikros_rt::log!(
            Error,
            "term-probe: the C program ended with {}",
            status.exit_code
        );
        return None;
    }
    check("the C program's last line", done == b"done\r\n")
}
