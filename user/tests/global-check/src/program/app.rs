//! The checks that run as the installed app `org.example.daemon`, whose
//! program is a copy of this one: an application, which reaches the
//! kernel and the system's services only through the C library's
//! functions (MIKSPEC-0068.R4, MIKSPEC-0068.R26).

use super::{DAEMON, Failed, SERVICE, ensure, failed};
use std::ffi::CString;
use std::process::Command;
use std::time::Duration;

/// The statuses of `<mikros/syscalls.h>` that the checks expect.
const OK: i64 = 0;
const ACCESS_DENIED: i64 = -13;
const ALREADY_EXISTS: i64 = -26;
/// `ServiceState.RUNNING` and `EXITED`.
const RUNNING: u32 = 2;
const EXITED: u32 = 4;
/// What the package's service writes when it starts.
pub const STARTED: &str = "idle: running";

#[repr(C)]
#[derive(Default)]
struct ServiceStatus {
    state: u32,
    pid: u32,
    restarts: u32,
    exit_status: i32,
    enabled: bool,
}

#[repr(C)]
struct LogRecord {
    sequence: u64,
    timestamp: i64,
    utc: i64,
    pid: u32,
    app_id: [u8; 129],
    severity: u32,
    message: [u8; 1025],
}

#[repr(C)]
struct Client {
    pid: u32,
    user: u32,
    app_id: [u8; 129],
    koid: u64,
}

unsafe extern "C" {
    fn mikros_public_create() -> i64;
    fn mikros_service_status(name: *const u8, status: *mut ServiceStatus) -> i64;
    fn mikros_service_stop(name: *const u8) -> i64;
    fn mikros_service_start(name: *const u8) -> i64;
    fn mikros_own_log_read(after: u64, records: *mut LogRecord, max: u32, count: *mut u32) -> i64;
    fn mikros_service_register(protocol: *const u8, instance: *const u8, provider: *mut u32)
    -> i64;
    fn mikros_service_connect(protocol: *const u8, instance: *const u8, server: u32) -> i64;
    fn mikros_service_accept(
        provider: u32,
        protocol: *mut u8,
        instance: *mut u8,
        server: *mut u32,
        client: *mut Client,
    ) -> i64;
    fn mikros_channel_create(options: u32, out0: *mut u32, out1: *mut u32) -> i64;
    fn mikros_handle_close(handle: u32) -> i64;
}

/// The NUL-terminated text of a fixed buffer.
fn text(bytes: &[u8]) -> &str {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).unwrap_or("")
}

fn c(text: &str) -> CString {
    CString::new(text).expect("no NUL")
}

/// The service's entry, or the status of the call.
fn status_of(name: &str) -> Result<ServiceStatus, i64> {
    let mut status = ServiceStatus::default();
    let name = c(name);
    // SAFETY: the pointers are valid for the call.
    match unsafe { mikros_service_status(name.as_ptr().cast(), &mut status) } {
        OK => Ok(status),
        error => Err(error),
    }
}

/// Waits until the service `name` is in `state`.
fn wait_state(name: &str, state: u32, code: u8) -> Result<(), Failed> {
    for _ in 0..60 {
        if status_of(name).is_ok_and(|s| s.state == state) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    failed(code, format!("{name} never reached state {state}"))
}

pub fn checks() -> Result<(), Failed> {
    public()?;
    services()?;
    logs()?;
    protocols()?;
    documents()
}

/// The note that the app writes in ada's documents folder, which it may
/// once she allowed it (MIKSPEC-0050.R38).
const NOTE: &str = "/users/ada/documents/global-check-note";

/// The documents folder, which ada allowed the app, at its real path.
fn documents() -> Result<(), Failed> {
    std::fs::write(NOTE, b"granted").or(failed(55, "writing to the documents folder"))?;
    let read = std::fs::read(NOTE).or(failed(55, "reading the note"))?;
    ensure(read == b"granted", 55, "the note")
}

/// As a second process of the app: holds the note open, says `ready`,
/// and once told, after ada denied the folder, finds the open file and
/// the folder out of reach.
pub fn hold() -> Result<(), Failed> {
    use std::io::{BufRead, Read};
    let mut file = std::fs::File::open(NOTE).or(failed(56, "opening the note"))?;
    println!("ready");
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    let mut bytes = Vec::new();
    ensure(
        file.read_to_end(&mut bytes).is_err(),
        56,
        "the open note after the revocation",
    )?;
    ensure(
        std::fs::read(NOTE).is_err(),
        57,
        "the folder after the revocation",
    )?;
    Ok(())
}

/// The process that starts before any folder is decided: it lists its
/// pending entries, and then does what it is told on its input, which
/// `pending.rs` drives.
pub fn pending() -> Result<(), Failed> {
    use std::io::{BufRead, Read};
    use std::os::unix::fs::PermissionsExt;
    let names: Vec<String> = std::fs::read_dir("/users/ada")
        .or(failed(67, "listing ada's home"))?
        .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    ensure(
        ["documents", "pictures"]
            .iter()
            .all(|n| names.iter().any(|name| name == n)),
        67,
        &format!("the pending entries in {names:?}"),
    )?;
    let shown = std::fs::metadata("/users/ada/pictures").or(failed(67, "the pictures entry"))?;
    ensure(
        shown.is_dir() && shown.permissions().mode() & 0o777 == 0o555,
        67,
        "the pictures entry before an answer",
    )?;
    println!("ready");
    let mut input = std::io::stdin().lock();
    let mut line = String::new();
    let mut next = |line: &mut String| {
        line.clear();
        let _ = input.read_line(line);
        line.trim().to_string()
    };
    ensure(next(&mut line) == "pictures", 68, "the first word")?;
    let refused = std::fs::File::open("/users/ada/pictures/none");
    ensure(
        refused
            .as_ref()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied),
        68,
        &format!("the denied pictures folder gave {refused:?}"),
    )?;
    println!("denied");
    ensure(next(&mut line) == "documents", 69, "the second word")?;
    documents().map_err(|Failed(_, what)| Failed(69, what))?;
    let mut open = std::fs::File::open(NOTE).or(failed(69, "opening the note"))?;
    println!("allowed");
    ensure(next(&mut line) == "after", 70, "the third word")?;
    let mut bytes = Vec::new();
    ensure(
        open.read_to_end(&mut bytes).is_err() && std::fs::read(NOTE).is_err(),
        70,
        "the note after the revocation",
    )?;
    println!("closed");
    Ok(())
}

/// Its public folder, made once, which a later process of the app
/// writes to (MIKSPEC-0101.R53).
fn public() -> Result<(), Failed> {
    // SAFETY: the call takes no pointers.
    let created = unsafe { mikros_public_create() };
    ensure(
        created == OK,
        40,
        &format!("mikros_public_create gave {created}"),
    )?;
    // SAFETY: as above.
    let again = unsafe { mikros_public_create() };
    ensure(
        again == ALREADY_EXISTS,
        41,
        &format!("a second one gave {again}"),
    )?;
    let program = format!("/apps/{DAEMON}/bin/idle");
    match Command::new(&program).arg("write").status() {
        Ok(status) if status.success() => Ok(()),
        other => failed(42, format!("writing the public folder: {other:?}")),
    }
}

/// As a later process of the app: writes a note to its public folder.
pub fn write_public() -> Result<(), Failed> {
    let note = format!("/public/{DAEMON}/note");
    std::fs::write(&note, b"shared").or(failed(43, "writing to the public folder"))?;
    let read = std::fs::read(&note).or(failed(43, "reading the note"))?;
    ensure(read == b"shared", 43, "the note")
}

/// Its package's service: listed as running, and stopped and started
/// again by the app without an administrator (MIKSPEC-0036.R52).
fn services() -> Result<(), Failed> {
    let running = status_of(SERVICE).is_ok_and(|s| s.state == RUNNING && s.pid > 0);
    ensure(running, 44, "the service's status")?;
    let name = c(SERVICE);
    // SAFETY: the pointer is valid for the call.
    let stopped = unsafe { mikros_service_stop(name.as_ptr().cast()) };
    ensure(
        stopped == OK,
        45,
        &format!("mikros_service_stop gave {stopped}"),
    )?;
    wait_state(SERVICE, EXITED, 45)?;
    // SAFETY: as above.
    let started = unsafe { mikros_service_start(name.as_ptr().cast()) };
    ensure(
        started == OK,
        46,
        &format!("mikros_service_start gave {started}"),
    )?;
    wait_state(SERVICE, RUNNING, 46)
}

/// Its own log records: the service's, and none of another app
/// (MIKSPEC-0063.R19). `Read` leaves another app's records out of each
/// batch of 32, so a batch may come back empty before the end; the next
/// one starts 32 records on.
fn logs() -> Result<(), Failed> {
    let mut after = 0;
    for _ in 0..2000 {
        let mut records: Vec<LogRecord> = Vec::with_capacity(32);
        let mut count = 0;
        // SAFETY: the buffer holds 32 records, and `count` is valid.
        let read = unsafe { mikros_own_log_read(after, records.as_mut_ptr(), 32, &mut count) };
        ensure(read == OK, 47, &format!("mikros_own_log_read gave {read}"))?;
        // SAFETY: the C library wrote `count` records.
        unsafe { records.set_len(count as usize) };
        let Some(last) = records.last() else {
            after += 32;
            continue;
        };
        after = last.sequence;
        let mut seen = false;
        for r in &records {
            let app_id = text(&r.app_id);
            ensure(app_id == DAEMON, 47, &format!("a record of {app_id}"))?;
            seen |= text(&r.message).contains(STARTED);
        }
        if seen {
            return Ok(());
        }
    }
    failed(47, "the service's record")
}

/// A protocol of its own, which it registers and serves, and one of
/// another app's, which it may not register (MIKSPEC-0101.R68,
/// MIKSPEC-0036.R50).
fn protocols() -> Result<(), Failed> {
    let (protocol, instance) = (c(&format!("{DAEMON}.Echo")), c("one"));
    let mut provider = 0;
    // SAFETY: the pointers are valid for the call.
    let registered = unsafe {
        mikros_service_register(
            protocol.as_ptr().cast(),
            instance.as_ptr().cast(),
            &mut provider,
        )
    };
    ensure(
        registered == OK,
        48,
        &format!("registering gave {registered}"),
    )?;
    let (mut ours, mut theirs) = (0, 0);
    // SAFETY: the out pointers are valid.
    let made = unsafe { mikros_channel_create(0, &mut ours, &mut theirs) };
    ensure(made == OK, 49, "a channel")?;
    // SAFETY: the pointers are valid; the call takes `theirs` over.
    let connected = unsafe {
        mikros_service_connect(protocol.as_ptr().cast(), instance.as_ptr().cast(), theirs)
    };
    ensure(connected == OK, 49, &format!("connecting gave {connected}"))?;
    let (mut name, mut named) = ([0u8; 129], [0u8; 65]);
    let mut server = 0;
    let mut client = Client {
        pid: 0,
        user: 0,
        app_id: [0; 129],
        koid: 0,
    };
    // SAFETY: the buffers have the sizes of the header.
    let accepted = unsafe {
        mikros_service_accept(
            provider,
            name.as_mut_ptr(),
            named.as_mut_ptr(),
            &mut server,
            &mut client,
        )
    };
    ensure(accepted == OK, 49, &format!("accepting gave {accepted}"))?;
    let expected = (text(&name), text(&named), text(&client.app_id));
    ensure(
        expected == (&format!("{DAEMON}.Echo")[..], "one", DAEMON),
        49,
        &format!("the connection {expected:?}"),
    )?;
    // SAFETY: the handles are ours.
    unsafe {
        mikros_handle_close(server);
        mikros_handle_close(ours);
    }
    let other = c("org.example.other.Echo");
    let mut unused = 0;
    // SAFETY: the pointers are valid for the call.
    let refused = unsafe {
        mikros_service_register(other.as_ptr().cast(), instance.as_ptr().cast(), &mut unused)
    };
    ensure(
        refused == ACCESS_DENIED,
        50,
        &format!("another app's protocol gave {refused}"),
    )
}
