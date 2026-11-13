//! The boot service of `tests/term.img`, the terminal service's client
//! for the C library: it runs the C program `term-c` on a terminal that
//! it opens through `Factory`, types what that program waits for and
//! checks what it shows. It exits with 0 once every check passed, and
//! logs what failed otherwise. The system test services-terminal checks
//! the terminal service itself as a client of the running one.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
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
const FACTORY: &str = "mikros.terminal.Factory";
const TERMINAL: &str = "mikros.terminal.Terminal";
/// The C program run on a terminal, and the descriptor kind of a slave
/// terminal.
const C_PROGRAM: &str = "/usr/bin/term-c";
const TERMINAL_KIND: u32 = 8;
const FOREVER: i64 = i64::MAX;
const SECOND: i64 = 1_000_000_000;
const CALLER_USER: u32 = u32::MAX;
const SAME_RIGHTS: u32 = 1 << 31;
const READABLE: u32 = 1 << 0;
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

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(managed) = managed::receive(bootstrap) else {
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
