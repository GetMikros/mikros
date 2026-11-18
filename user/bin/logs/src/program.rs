//! logs on Mikros: it reads the log service's `Reader`, whose connection
//! needs `admin.logs.read`, for which the directory asks an
//! administrator, from the first record and, when asked, follows it.

use crate::listing::{self, COMMAND, Entry, Filter, Request};
use mikros_cli::{FAILURE, INVALID, Paint, Run, SUCCESS, complain, start};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::log::{Reader, ReaderClient, ReaderEvent, Record, Severity};
use std::collections::VecDeque;
use std::io::Write;
use std::process::ExitCode;

const READER: &str = "mikros.log.Reader";
/// The most records one `Read` gives.
const BATCH: u32 = 32;
const FOREVER: i64 = i64::MAX;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let fail = |status: u8, message: &str| fail(&run, status, message);
    let Request {
        filter,
        last,
        follow,
        json,
    } = match listing::read(&run) {
        Ok(request) => request,
        Err(reason) => return fail(INVALID, &reason),
    };
    let paint = Paint::stdout(&run);
    let Some(directory) = Directory::new(std::os::mikros::directory().as_raw_handle()) else {
        return fail(FAILURE, "cannot reach the service directory");
    };
    let reader = match directory.connect_unless_denied::<Reader>(READER) {
        Ok(Some(client)) => ReaderClient::new(client),
        Ok(None) => return fail(FAILURE, "not authorized"),
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
            .try_for_each(|e| writeln!(out, "{}", listing::line(e, paint)))
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
            if filter.matches(&entry) && writeln!(out, "{}", listing::line(&entry, paint)).is_err()
            {
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
fn fail(run: &Run, status: u8, message: &str) -> ExitCode {
    complain(&COMMAND, run, message);
    ExitCode::from(status)
}
