//! logs on Mikros: it asks the session manager's `Authorization` with
//! its own identity, and once an administrator allowed it, reads the log
//! service's `Reader` from the first record and, when asked, follows it.

use crate::listing::{self, COMMAND, Entry, Filter, Request};
use mikros_cli::{FAILURE, INVALID, SUCCESS};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::log::{Reader, ReaderClient, ReaderEvent, Record, Severity};
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use std::collections::VecDeque;
use std::io::Write;
use std::process::ExitCode;

const READER: &str = "mikros.log.Reader";
const APP_ID: &str = "system.logs";
const ACTION: &str = "logs.read";
/// The most records one `Read` gives.
const BATCH: u32 = 32;
const FOREVER: i64 = i64::MAX;

unsafe extern "C" {
    fn getuid() -> u32;
}

pub fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let (filter, last, follow, json) = match listing::read(arguments.clone()) {
        Request::Print {
            filter,
            last,
            follow,
            json,
        } => (filter, last, follow, json),
        Request::Help => {
            let _ = std::io::stdout().write_all(COMMAND.help().as_bytes());
            return ExitCode::from(SUCCESS);
        }
        Request::Invalid(reason) => return fail(INVALID, &reason),
    };
    let Some(directory) = Directory::new(std::os::mikros::directory().as_raw_handle()) else {
        return fail(FAILURE, "cannot reach the service directory");
    };
    if !authorized(&directory, &arguments) {
        return fail(FAILURE, "not authorized");
    }
    let reader = match directory.connect::<Reader>(READER) {
        Ok(client) => ReaderClient::new(client),
        Err(why) => return fail(FAILURE, &why),
    };
    // Following before reading leaves out no record that comes while
    // the log is read; those read twice are printed once.
    if follow && !matches!(reader.follow(FOREVER), Ok(Ok(_))) {
        return fail(FAILURE, "the log cannot be followed");
    }
    let (mut entries, mut latest) = match read_all(&reader, &filter, last) {
        Ok(read) => read,
        Err(why) => return fail(FAILURE, &why),
    };
    let mut out = std::io::stdout().lock();
    let written = if json {
        out.write_all(listing::json(entries.make_contiguous()).as_bytes())
    } else {
        entries
            .iter()
            .try_for_each(|e| writeln!(out, "{}", listing::line(e)))
    };
    if written.and_then(|()| out.flush()).is_err() {
        return ExitCode::from(FAILURE);
    }
    if !follow {
        return ExitCode::from(SUCCESS);
    }
    while let Ok(ReaderEvent::OnRecords(event)) = reader.next_event(FOREVER) {
        for entry in event.records.into_iter().map(entry) {
            if entry.sequence <= latest {
                continue;
            }
            latest = entry.sequence;
            if filter.matches(&entry) && writeln!(out, "{}", listing::line(&entry)).is_err() {
                return ExitCode::from(FAILURE);
            }
        }
        if out.flush().is_err() {
            return ExitCode::from(FAILURE);
        }
    }
    fail(FAILURE, "the log service ended the connection")
}

/// The records from the first that `filter` matches, only the `last` of
/// them when that is given, and the sequence number of the latest record
/// read.
fn read_all(
    reader: &ReaderClient,
    filter: &Filter,
    last: Option<usize>,
) -> Result<(VecDeque<Entry>, u64), String> {
    let mut entries = VecDeque::new();
    let mut latest = 0;
    loop {
        let read = reader
            .read(latest, BATCH, FOREVER)
            .map_err(|e| format!("Read failed: {e:?}"))?;
        if read.records.is_empty() {
            return Ok((entries, latest));
        }
        for entry in read.records.into_iter().map(entry) {
            latest = latest.max(entry.sequence);
            if !filter.matches(&entry) {
                continue;
            }
            match last {
                Some(0) => continue,
                Some(n) if entries.len() == n => {
                    entries.pop_front();
                }
                _ => {}
            }
            entries.push_back(entry);
        }
    }
}

fn entry(record: Record) -> Entry {
    Entry {
        sequence: record.sequence,
        utc: record.utc,
        pid: record.pid,
        app_id: record.app_id,
        severity: match record.severity {
            Severity::Debug => 0,
            Severity::Info => 1,
            Severity::Warning => 2,
            Severity::Error => 3,
            Severity::Unknown(n) => n,
        },
        message: record.message,
    }
}

/// Writes the error `message` to standard error and gives `status`.
fn fail(status: u8, message: &str) -> ExitCode {
    eprintln!("{}", COMMAND.error(message));
    ExitCode::from(status)
}

/// Whether an administrator allows `logs.read` for this process, asked
/// with the command line as the reason.
fn authorized(directory: &Directory, arguments: &[String]) -> bool {
    let me = ClientIdentity {
        pid: std::process::id(),
        // SAFETY: the call takes no pointers.
        user: unsafe { getuid() },
        app_id: APP_ID.into(),
        admin: 0,
    };
    let reason = format!("logs {}", arguments.join(" "));
    directory.authorized(me, ACTION, reason.trim_end().into())
}
