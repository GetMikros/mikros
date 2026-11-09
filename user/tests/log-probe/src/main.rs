//! A program that the service manager starts as a boot service after the
//! log service, to drive it as a client. It writes records through
//! `mikros.log.Writer` and lines to its standard output and error, and
//! reads them back through `mikros.log.Reader`, which its permission
//! `logs.read` lets it use, with `Read` and with `Follow`. It exits with
//! 0 when every check passed, or with the number of the first that
//! failed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::log::{
    Reader, ReaderClient, ReaderEvent, Record, Severity, Writer, WriterClient,
};
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
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
    let me = (managed.header.pid, managed.header.app_id.clone());
    let directory = DirectoryClient::new(managed.header.directory);
    let result = check(&directory, &me).and_then(|()| streams(&directory, &me, &managed.handles));
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
    let kernel = records
        .iter()
        .any(|r| r.app_id == "kernel" && r.pid == 0 && r.message.contains("started service log"));
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
