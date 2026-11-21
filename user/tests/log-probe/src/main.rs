//! A program that the service manager starts as a boot service after the
//! log service, to drive it as a client. It writes records through
//! `mikros.log.Writer` and lines to its standard output and error, and
//! reads them back through `mikros.log.Reader`, which its permission
//! `admin.logs.read` lets it use, with `Read` and with `Follow`. Then it
//! starts itself with the argument `child` and no standard handles, so
//! that the process manager gives the child its standard output and
//! error, and finds the line that the child writes to each. It exits
//! with 0 when every check passed, or with the number of the first that
//! failed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::log::{
    Reader, ReaderClient, ReaderEvent, Record, Severity, Writer, WriterClient,
};
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherClient, SpawnRequest, StartupHandle, WaitOptions,
};
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const FOREVER: i64 = i64::MAX;
/// How long a record may take to reach the log, emulation included.
const WAIT: i64 = 60_000_000_000;
const FIRST: &str = "log-probe: the first record";
const FOLLOWED: &str = "log-probe: a record to follow";
const OUTPUT: &str = "log-probe: a line of standard output";
const ERROR: &str = "log-probe: a line of standard error";
const CHILD_OUTPUT: &str = "log-probe: the child's line of standard output";
const CHILD_ERROR: &str = "log-probe: the child's line of standard error";
const CHILD: &str = "child";
/// The user of a `Spawn` that keeps the caller's.
const CALLER_USER: u32 = u32::MAX;

/// Waits a little before reading again.
fn pause() {
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_sleep(now() + 10_000_000) };
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor, |_| {}));
    let Ok(managed) = received else {
        return PANIC_EXIT_CODE;
    };
    // The child writes a line to the standard output and error that the
    // process manager gave it.
    if arguments(&managed.header.arguments)
        .get(1)
        .map(String::as_str)
        == Some(CHILD)
    {
        let said = say(&managed.handles, HandleRole::Stdout, CHILD_OUTPUT)
            .and_then(|()| say(&managed.handles, HandleRole::Stderr, CHILD_ERROR));
        return if said.is_some() { 0 } else { 30 };
    }
    let me = (managed.header.pid, managed.header.app_id.clone());
    let program = managed.header.program.clone();
    let directory = DirectoryClient::new(managed.header.directory);
    let launcher = LauncherClient::new(managed.header.launcher);
    let result = check(&directory, &me)
        .and_then(|()| streams(&directory, &me, &managed.handles))
        .and_then(|()| given(&directory, &launcher, &program));
    match result {
        Ok(()) => 0,
        Err(failed) => failed,
    }
}

/// Writes `line` and a line feed to the stream of `role` among `handles`.
fn say(handles: &[StartupHandle], role: HandleRole, line: &str) -> Option<()> {
    let stream = handles.iter().find(|h| h.role == role)?.handle.raw();
    let mut bytes = Vec::from(line.as_bytes());
    bytes.push(b'\n');
    let mut written = 0;
    // SAFETY: the pointers describe `bytes` and `written`.
    let status =
        unsafe { sys::stream_write(stream, bytes.as_ptr(), bytes.len() as u64, &mut written) };
    (status == mikros_rt::OK && written == bytes.len() as u64).then_some(())
}

/// Writes a line to the standard output and one to the standard error
/// that the service manager gave, and finds them as records of severity
/// INFO and ERROR with this program's pid and app ID.
fn streams(
    directory: &DirectoryClient,
    me: &(u32, String),
    handles: &[StartupHandle],
) -> Result<(), i64> {
    say(handles, HandleRole::Stdout, OUTPUT).ok_or(11)?;
    say(handles, HandleRole::Stderr, ERROR).ok_or(12)?;
    let (reader, server) = endpoints::<Reader>().map_err(|_| 13)?;
    match directory.connect(
        "mikros.log.Reader".into(),
        None,
        server.into_handle(),
        FOREVER,
    ) {
        Ok(Ok(_)) => {}
        _ => return Err(13),
    }
    let reader = ReaderClient::new(reader);
    let found = |records: &[Record], message: &str, severity: Severity| {
        records.iter().any(|r| {
            r.message == message && r.pid == me.0 && r.app_id == me.1 && r.severity == severity
        })
    };
    let deadline = now() + WAIT;
    let mut records: Vec<Record> = Vec::new();
    loop {
        if found(&records, OUTPUT, Severity::Info) && found(&records, ERROR, Severity::Error) {
            return Ok(());
        }
        if now() > deadline {
            return Err(14);
        }
        let after = records.last().map_or(0, |r| r.sequence);
        let read = reader.read(after, 32, FOREVER).map_err(|_| 14)?;
        if read.records.is_empty() {
            pause();
        }
        records.extend(read.records);
    }
}

/// The arguments in the memory object `memory`.
fn arguments(memory: &OwnedHandle) -> Vec<String> {
    let mut bytes = alloc::vec![0u8; 4096];
    // SAFETY: the pointer describes `bytes`.
    let read = unsafe { sys::mem_read(memory.raw(), 0, bytes.as_mut_ptr(), bytes.len() as u64) };
    if read != mikros_rt::OK {
        return Vec::new();
    }
    let count = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    bytes[8..]
        .split(|&b| b == 0)
        .take(count)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
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
    let mut memory = 0;
    let size = (bytes.len() as u64).next_multiple_of(4096);
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::mem_create(size, 0, &mut memory) } != mikros_rt::OK {
        return None;
    }
    // SAFETY: the kernel just created the memory object for us.
    let memory = unsafe { OwnedHandle::from_raw(memory) };
    // SAFETY: the pointer and length describe `bytes`.
    let written = unsafe { sys::mem_write(memory.raw(), 0, bytes.as_ptr(), bytes.len() as u64) };
    (written == mikros_rt::OK).then_some(memory)
}

/// Starts this program, `program`, as a child without standard handles,
/// waits until it exits with 0, and finds the lines that it wrote to the
/// standard output and error that the process manager gave it: records
/// of severity INFO and ERROR with the child's pid that the log service
/// made of them, or, from before the process manager reached the log
/// service, the kernel log's lines that it wrote of them, after the
/// child's app ID and pid.
fn given(directory: &DirectoryClient, launcher: &LauncherClient, program: &str) -> Result<(), i64> {
    let request = SpawnRequest {
        path: program.into(),
        arguments: argument_memory(&[program, CHILD]).ok_or(20)?,
        handles: Vec::new(),
        app_id: None,
        user: CALLER_USER,
        login_session: false,
        new_session: false,
        working_directory: None,
        umask: u32::MAX,
    };
    let spawned = match launcher.spawn(request, FOREVER) {
        Ok(Ok(spawned)) => spawned,
        _ => return Err(21),
    };
    let child = spawned.pid;
    match launcher.wait(child as i32, WaitOptions(0), FOREVER, FOREVER) {
        Ok(Ok(ended))
            if ended.pid == child
                && ended.status.exit_code == 0
                && ended.status.term_signal == 0 => {}
        _ => return Err(22),
    }
    let (reader, server) = endpoints::<Reader>().map_err(|_| 23)?;
    match directory.connect(
        "mikros.log.Reader".into(),
        None,
        server.into_handle(),
        FOREVER,
    ) {
        Ok(Ok(_)) => {}
        _ => return Err(23),
    }
    let reader = ReaderClient::new(reader);
    let found = |records: &[Record], line: &str, severity: Severity| {
        records.iter().any(|r| {
            let added = r.pid == child && r.message == line;
            let written = r.message.ends_with(&format!(" {child}: {line}"));
            r.severity == severity && (added || written)
        })
    };
    let deadline = now() + WAIT;
    let mut records: Vec<Record> = Vec::new();
    loop {
        let output = found(&records, CHILD_OUTPUT, Severity::Info);
        if output && found(&records, CHILD_ERROR, Severity::Error) {
            return Ok(());
        }
        if now() > deadline {
            return Err(24);
        }
        let after = records.last().map_or(0, |r| r.sequence);
        let read = reader.read(after, 32, FOREVER).map_err(|_| 24)?;
        if read.records.is_empty() {
            pause();
        }
        records.extend(read.records);
    }
}

/// Whether `record` is the record with `message` that this program wrote,
/// with its pid and app ID.
fn ours(record: &Record, me: &(u32, String), message: &str) -> bool {
    record.message == message
        && record.pid == me.0
        && record.app_id == me.1
        && record.severity == Severity::Info
}

fn check(directory: &DirectoryClient, me: &(u32, String)) -> Result<(), i64> {
    let (writer, server) = endpoints::<Writer>().map_err(|_| 1)?;
    match directory.connect(
        "mikros.log.Writer".into(),
        None,
        server.into_handle(),
        FOREVER,
    ) {
        Ok(Ok(_)) => {}
        _ => return Err(1),
    }
    let writer = WriterClient::new(writer);
    writer.write(Severity::Info, FIRST.into()).map_err(|_| 2)?;
    let (reader, server) = endpoints::<Reader>().map_err(|_| 3)?;
    match directory.connect(
        "mikros.log.Reader".into(),
        None,
        server.into_handle(),
        FOREVER,
    ) {
        Ok(Ok(_)) => {}
        _ => return Err(3),
    }
    let reader = ReaderClient::new(reader);

    // Every record from the first, until the probe's own shows up.
    let deadline = now() + WAIT;
    let mut records: Vec<Record> = Vec::new();
    while !records.iter().any(|r| ours(r, me, FIRST)) {
        if now() > deadline {
            return Err(4);
        }
        let after = records.last().map_or(0, |r| r.sequence);
        let read = reader.read(after, 32, FOREVER).map_err(|_| 4)?;
        if read.records.len() > 32 {
            return Err(5);
        }
        if read.records.is_empty() {
            pause();
        }
        records.extend(read.records);
    }
    // Sequence numbers count from 1, one by one.
    let numbered = records
        .iter()
        .enumerate()
        .all(|(i, r)| r.sequence == i as u64 + 1);
    if !numbered {
        return Err(6);
    }
    // The kernel log's records, such as the service manager's, come with
    // the app ID `kernel` and no pid.
    let kernel = records.iter().any(|r| {
        r.app_id == "kernel" && r.pid == 0 && r.message.contains("started service logsvc")
    });
    if !kernel {
        return Err(7);
    }

    // After Follow, records come as events.
    match reader.follow(FOREVER) {
        Ok(Ok(_)) => {}
        _ => return Err(8),
    }
    writer
        .write(Severity::Info, FOLLOWED.into())
        .map_err(|_| 9)?;
    let deadline = now() + WAIT;
    loop {
        let Ok(ReaderEvent::OnRecords(event)) = reader.next_event(deadline) else {
            return Err(10);
        };
        if event.records.iter().any(|r| ours(r, me, FOLLOWED)) {
            return Ok(());
        }
    }
}
