//! The terminal service, as a system test: a client of the running one
//! opens terminals through `Factory` and checks what `Terminal` reads,
//! writes, echoes and signals, its limits, job control on a terminal that
//! the test makes its controlling terminal, and the numbers of terminals
//! whose master closed. Copies of the test, started with an argument, are
//! the processes that job control acts on: `sleep` waits until it is
//! killed, and `read` reads the terminal whose token it gets as its `USER`
//! startup handle 1.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use mikros_rt::Managed;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherClient, SpawnRequest, StartupHandle, StatusKind, WaitOptions,
};
use mikros_rt::interfaces::mikros::terminal::{
    Factory, FactoryClient, Terminal, TerminalClient, WindowSize,
};
use mikros_rt::sys;
use mikros_test::{Failure, Own, ensure};

mikros_rt::managed_main!(main);

const TOKEN_ID: u32 = 1;
const FACTORY: &str = "mikros.terminal.Factory";
const TERMINAL: &str = "mikros.terminal.Terminal";
const BINARY: &str = "/usr/bin/tests/services-terminal";
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

type Result<T = ()> = mikros_test::Result<T>;

fn main(m: &Managed) -> i64 {
    match arguments(&m.header.arguments).get(1).map(String::as_str) {
        Some("sleep") => loop {
            // SAFETY: the call takes no pointers.
            unsafe { sys::thread_sleep(FOREVER) };
        },
        Some("read") => return read_in_background(m),
        _ => {}
    }
    mikros_test::run(
        m,
        &[
            ("lines", lines),
            ("limits", limits),
            ("job_control", job_control),
            ("closing_the_master", closing_the_master),
        ],
    )
}

/// As the copy `read`: reads the terminal of its token, which a
/// background read stops before it returns. Exits with 0 when the read
/// was interrupted.
fn read_in_background(m: &Managed) -> i64 {
    let token = m
        .handles
        .iter()
        .find(|h| h.role == HandleRole::User && h.id == TOKEN_ID)
        .and_then(|h| duplicate(&h.handle));
    let own = Own::of(m);
    let (Some(token), Ok(terminal)) = (token, connect::<Terminal>(&own, TERMINAL)) else {
        return 3;
    };
    let read = TerminalClient::new(terminal).read(token, 10, FOREVER, FOREVER);
    if matches!(read, Ok(Err(Status::Interrupted))) {
        0
    } else {
        4
    }
}

fn failed<T>(what: &'static str) -> Result<T> {
    Err(Failure { what, status: -1 })
}

/// The reply of the call `what`.
fn reply<T, E, F>(
    what: &'static str,
    r: core::result::Result<core::result::Result<T, E>, F>,
) -> Result<T> {
    match r {
        Ok(Ok(t)) => Ok(t),
        _ => failed(what),
    }
}

fn now() -> i64 {
    mikros_test::now()
}

fn connect<P>(s: &Own, protocol: &str) -> Result<ClientEnd<P>> {
    let Ok((client, server)) = endpoints::<P>() else {
        return failed("making a channel");
    };
    let connected = s
        .services()
        .connect(protocol.into(), None, server.into_handle(), FOREVER);
    ensure(
        "connecting to the terminal service",
        matches!(connected, Ok(Ok(_))),
    )?;
    Ok(client)
}

/// The terminal service's `Factory` and `Terminal`.
fn service(s: &Own) -> Result<(FactoryClient, TerminalClient)> {
    let factory = FactoryClient::new(connect::<Factory>(s, FACTORY)?);
    let terminal = TerminalClient::new(connect::<Terminal>(s, TERMINAL)?);
    Ok((factory, terminal))
}

/// A new terminal of `rows` by `columns`: its master stream and token.
fn open(factory: &FactoryClient, rows: u16, columns: u16) -> Result<(OwnedHandle, OwnedHandle)> {
    let size = WindowSize {
        rows,
        columns,
        width: 0,
        height: 0,
    };
    let opened = reply("Open", factory.open(size, FOREVER))?;
    Ok((opened.master, opened.terminal))
}

fn duplicate(handle: &OwnedHandle) -> Option<OwnedHandle> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle.raw(), SAME_RIGHTS, &mut out) };
    // SAFETY: the kernel just created the handle for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
}

/// A copy of the terminal token `token`.
fn copy(token: &OwnedHandle) -> Result<OwnedHandle> {
    duplicate(token).ok_or(Failure {
        what: "copying the token",
        status: -1,
    })
}

/// The signals of `handle` that are set now, of `signals`.
fn signals(handle: &OwnedHandle, signals: u32, deadline: i64) -> u32 {
    let mut observed = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::object_wait_one(handle.raw(), signals, deadline, &mut observed) };
    observed & signals
}

/// Types `bytes` on the master stream.
fn type_in(master: &OwnedHandle, bytes: &[u8]) -> Result {
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
    ensure(
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

/// Starts a copy of the test with `mode` and `handles`, in a process
/// group of its own.
fn child(launcher: &LauncherClient, mode: &str, handles: Vec<StartupHandle>) -> Result<u32> {
    let request = SpawnRequest {
        path: BINARY.into(),
        arguments: argument_memory(&[BINARY, mode]).ok_or(Failure {
            what: "writing the arguments",
            status: -1,
        })?,
        handles,
        app_id: None,
        user: CALLER_USER,
        login_session: false,
        new_session: false,
        working_directory: None,
        umask: u32::MAX,
    };
    let pid = reply("starting a copy", launcher.spawn(request, FOREVER))?.pid;
    reply(
        "SetProcessGroup",
        launcher.set_process_group(pid, pid, FOREVER),
    )?;
    Ok(pid)
}

/// A new terminal has the default attributes and its window size; a
/// typed line is read as NL and echoed with CR NL, and the terminal says
/// when it can be read and written; a written NL is shown as CR NL.
fn lines(s: &Own) -> Result {
    let (factory, terminal) = service(s)?;
    let (master, token) = open(&factory, 24, 80)?;
    let a = reply(
        "GetAttributes",
        terminal.get_attributes(copy(&token)?, FOREVER),
    )?
    .attributes;
    ensure(
        "the default attributes",
        a.local_flags == 0o105073 && a.input_flags == 0o2400,
    )?;
    let size = reply(
        "GetWindowSize",
        terminal.get_window_size(copy(&token)?, FOREVER),
    )?
    .size;
    ensure("the window size", size.rows == 24 && size.columns == 80)?;
    let readiness = reply(
        "GetReadiness",
        terminal.get_readiness(copy(&token)?, FOREVER),
    )?
    .event;
    ensure(
        "being writable",
        signals(&readiness, USER0 | USER1, 0) == USER1,
    )?;
    type_in(&master, b"hi\r")?;
    ensure("the echo", shown(&master, 4) == b"hi\r\n")?;
    ensure(
        "being readable",
        signals(&readiness, USER0, now() + SECOND) == USER0,
    )?;
    let read = reply("Read", terminal.read(copy(&token)?, 100, FOREVER, FOREVER))?.data;
    ensure("reading the line", read == b"hi\n")?;
    let write = terminal.write(copy(&token)?, b"a\nb".to_vec(), FOREVER, FOREVER);
    let written = reply("Write", write)?.written;
    ensure("writing", written == 3 && shown(&master, 4) == b"a\r\nb")
}

/// Reads that may not wait, time out, ask for too much or for nothing,
/// and a token of no terminal.
fn limits(s: &Own) -> Result {
    let (factory, terminal) = service(s)?;
    let (_master, token) = open(&factory, 24, 80)?;
    let read = |len, deadline| match terminal.read(copy(&token)?, len, deadline, FOREVER) {
        Ok(r) => Ok(r),
        Err(_) => failed("Read"),
    };
    ensure(
        "a read that may not wait",
        read(10, 0)? == Err(Status::ShouldWait),
    )?;
    ensure(
        "a read that times out",
        read(10, now() + 50 * MS)? == Err(Status::TimedOut),
    )?;
    ensure(
        "refusing a long read",
        read(40000, FOREVER)? == Err(Status::OutOfRange),
    )?;
    ensure(
        "a read of nothing",
        read(0, FOREVER)?.is_ok_and(|r| r.data.is_empty()),
    )?;
    let mut other = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::event_create(0, &mut other) };
    // SAFETY: the kernel just created the handle for us.
    let other = unsafe { OwnedHandle::from_raw(other) };
    let r = terminal.get_attributes(other, FOREVER);
    ensure(
        "refusing a token of no terminal",
        matches!(r, Ok(Err(Status::BadHandle))),
    )
}

/// The test leads a session with a terminal as its controlling terminal,
/// and no second one; ^C interrupts the foreground group, and a read from
/// a background group stops its reader with TTIN.
fn job_control(s: &Own) -> Result {
    let (factory, terminal) = service(s)?;
    let launcher = s.launcher();
    let (master, token) = open(&factory, 24, 80)?;
    reply("CreateSession", launcher.create_session(FOREVER))?;
    reply(
        "MakeControlling",
        terminal.make_controlling(copy(&token)?, FOREVER),
    )?;
    let again = terminal.make_controlling(copy(&token)?, FOREVER);
    ensure(
        "refusing a second controlling terminal",
        matches!(again, Ok(Err(Status::AccessDenied))),
    )?;
    let me = reply(
        "GetForeground",
        terminal.get_foreground(copy(&token)?, FOREVER),
    )?
    .pgid;

    let sleeper = child(&launcher, "sleep", Vec::new())?;
    reply(
        "SetForeground",
        terminal.set_foreground(copy(&token)?, sleeper, FOREVER),
    )?;
    type_in(&master, b"\x03")?;
    let waited = launcher.wait(sleeper as i32, WaitOptions(0), now() + 5 * SECOND, FOREVER);
    ensure(
        "interrupting the foreground group",
        reply("Wait", waited)?.status.term_signal == SIGINT,
    )?;
    reply(
        "SetForeground back",
        terminal.set_foreground(copy(&token)?, me, FOREVER),
    )?;

    let handles = vec![StartupHandle {
        role: HandleRole::User,
        id: TOKEN_ID,
        handle: copy(&token)?,
    }];
    let reader = child(&launcher, "read", handles)?;
    let waited = launcher.wait(
        reader as i32,
        WaitOptions::STOPPED,
        now() + 5 * SECOND,
        FOREVER,
    );
    let status = reply("Wait", waited)?.status;
    let stopped = status.kind == StatusKind::Stopped && status.stop_signal == SIGTTIN;
    let _ = launcher.kill(reader, SIGKILL, FOREVER);
    let _ = launcher.wait(reader as i32, WaitOptions(0), now() + 5 * SECOND, FOREVER);
    // The terminal is the test's controlling terminal for as long as it
    // runs: closing its master would hang it up and send HUP to the test,
    // which leads its session.
    core::mem::forget(master);
    ensure("stopping a background reader", stopped)
}

/// Input left when the master closes is read, then the end of input; the
/// number of a terminal whose master closed goes to the next terminal
/// opened, and a terminal opened after another gets a higher number.
fn closing_the_master(s: &Own) -> Result {
    let (factory, terminal) = service(s)?;
    let number = |token: &OwnedHandle| -> Result<u32> {
        Ok(reply("GetNumber", terminal.get_number(copy(token)?, FOREVER))?.number)
    };
    let (_first_master, first) = open(&factory, 1, 1)?;
    let (master, second) = open(&factory, 1, 1)?;
    let second_number = number(&second)?;
    ensure(
        "numbering terminals upwards",
        second_number > number(&first)?,
    )?;
    type_in(&master, b"z\n")?;
    drop(master);
    let deadline = now() + SECOND;
    let read = |what| Ok(reply(what, terminal.read(copy(&second)?, 10, deadline, FOREVER))?.data);
    ensure("reading after the master closed", read("Read")? == b"z\n")?;
    ensure(
        "the end of input after the master closed",
        read("Read at the end")?.is_empty(),
    )?;
    let (_third_master, third) = open(&factory, 1, 1)?;
    ensure(
        "reusing a closed terminal's number",
        number(&third)? == second_number,
    )
}
