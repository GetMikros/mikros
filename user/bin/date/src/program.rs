//! date on Mikros: the time comes from the standard library's clock or a
//! file's modified time, local times and their zones from the C
//! library's `localtime_r` and `mktime`, in the system's time zone unless
//! `TZ` names another (MIKSPEC-0033.R42), and `-s` sets
//! the time with the time service's `SetUtc` (MIKSPEC-0054), which asks
//! for `admin.time.set` itself.

use crate::format::Moment;
use crate::parse::{self, Given};
use crate::request::{self, COMMAND, Output, Request, Source};
use mikros_cli::{FAILURE, INVALID, Paint, Parsed, Role, Run, SUCCESS, complain, start, time};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::time::ClockClient;
use std::ffi::{CStr, c_char, c_int, c_long};
use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

const CLOCK: &str = "mikros.time.Clock";
const FOREVER: i64 = i64::MAX;
const NANOS: i64 = 1_000_000_000;

/// The C library's `struct tm`.
#[repr(C)]
struct Tm {
    sec: c_int,
    min: c_int,
    hour: c_int,
    mday: c_int,
    mon: c_int,
    year: c_int,
    wday: c_int,
    yday: c_int,
    isdst: c_int,
    gmtoff: c_long,
    zone: *const c_char,
}

unsafe extern "C" {
    fn localtime_r(time: *const i64, tm: *mut Tm) -> *mut Tm;
    fn mktime(tm: *mut Tm) -> i64;
}

/// Why a run failed, and the status it exits with.
type Failure = (u8, String);

pub fn main() -> ExitCode {
    let run = begin();
    let request = match request::request(&run) {
        Ok(request) => request,
        Err(why) => return fail(&run, (INVALID, why)),
    };
    let (seconds, nanos) = match when(&request) {
        Ok(when) => when,
        Err(failure) => return fail(&run, failure),
    };
    if let Source::Set(_) = request.source
        && let Err(failure) = set(seconds, nanos)
    {
        return fail(&run, failure);
    }
    let moment = moment(seconds, nanos, request.utc);
    let text = request::written(&moment, &request.output);
    let text = match request.output {
        Output::Json | Output::Format(_) => text,
        _ => format!(
            "{}\n",
            Paint::stdout(&run).paint(Role::Secondary, text.trim_end_matches('\n'))
        ),
    };
    match std::io::stdout().write_all(text.as_bytes()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(_) => ExitCode::from(FAILURE),
    }
}

fn fail(run: &Run, (status, why): Failure) -> ExitCode {
    complain(&COMMAND, run, &why);
    ExitCode::from(status)
}

/// The run, as `start` gives it, but with `-I` and `--iso-8601` given
/// their default value first, which the shared tables cannot leave out.
fn begin() -> Run {
    if std::env::var("MSH_COMPLETE").is_ok_and(|v| v == "1") {
        // It answers completion mode and exits.
        start(&COMMAND, &mikros_command::list);
    }
    let arguments = request::with_iso_default(std::env::args().skip(1).collect());
    match COMMAND.parse(arguments) {
        Parsed::Run(run) => run,
        Parsed::Help(path) => {
            let _ = std::io::stdout().write_all(COMMAND.help(&path).as_bytes());
            std::process::exit(i32::from(SUCCESS));
        }
        Parsed::Invalid(why) => {
            let no_color = std::env::var("NO_COLOR").ok();
            let paint = Paint::new(None, std::io::stderr().is_terminal(), no_color.as_deref());
            eprintln!("{}: {}", COMMAND.name, paint.paint(Role::Error, &why));
            std::process::exit(i32::from(INVALID));
        }
    }
}

/// The seconds since 1970 and nanoseconds of the time that `request`
/// shows.
fn when(request: &Request) -> Result<(i64, u32), Failure> {
    match &request.source {
        Source::Now => Ok(split(SystemTime::now())),
        Source::Date(text) | Source::Set(text) => resolve(text, request.utc),
        Source::Reference(file) => match std::fs::metadata(file).and_then(|m| m.modified()) {
            Ok(modified) => Ok(split(modified)),
            Err(e) => Err((FAILURE, format!("{file}: {e}"))),
        },
    }
}

/// `time` as seconds since 1970 and nanoseconds after them.
fn split(time: SystemTime) -> (i64, u32) {
    let seconds = time::seconds(time);
    let nanos = match time.duration_since(UNIX_EPOCH) {
        Ok(after) => after.subsec_nanos(),
        Err(before) => match before.duration().subsec_nanos() {
            0 => 0,
            n => 1_000_000_000 - n,
        },
    };
    // `seconds` rounds toward 1970; a time before it with nanoseconds
    // lies in the second before.
    match time < UNIX_EPOCH && nanos != 0 {
        true => (seconds - 1, nanos),
        false => (seconds, nanos),
    }
}

/// The time that the date `text` names: a local time in UTC with `utc`,
/// and otherwise in the local zone, on today's date when it has none.
fn resolve(text: &str, utc: bool) -> Result<(i64, u32), Failure> {
    let invalid = || (INVALID, format!("invalid date {text:?}"));
    let (date, (hour, minute, second, nanos), offset) =
        match parse::given(text).ok_or_else(invalid)? {
            Given::Now => return Ok(split(SystemTime::now())),
            Given::Epoch(seconds, nanos) => return Ok((seconds, nanos)),
            Given::Civil { date, time, offset } => (date, time, offset),
        };
    let date = match date {
        Some(date) => date,
        None => today(utc),
    };
    let offset = offset.or(utc.then_some(0));
    let seconds = match offset {
        Some(offset) => parse::utc_of(date, (hour, minute, second), offset),
        None => local_to_utc(date, (hour, minute, second)).ok_or_else(invalid)?,
    };
    Ok((seconds, nanos))
}

/// Today's date, in UTC with `utc` and otherwise in local time.
fn today(utc: bool) -> (i64, u32, u32) {
    let (seconds, nanos) = split(SystemTime::now());
    let m = moment(seconds, nanos, utc);
    let local = m.seconds + i64::from(m.offset);
    mikros_tz::civil(local.div_euclid(86_400))
}

/// The seconds since 1970 of a local time, from the C library's
/// `mktime`.
fn local_to_utc(date: (i64, u32, u32), time: (u32, u32, u32)) -> Option<i64> {
    let (year, month, day) = date;
    let (hour, minute, second) = time;
    let mut tm = Tm {
        sec: second as c_int,
        min: minute as c_int,
        hour: hour as c_int,
        mday: day as c_int,
        mon: month as c_int - 1,
        year: c_int::try_from(year - 1900).ok()?,
        wday: 0,
        yday: 0,
        // Whether daylight saving time applies is for mktime to find.
        isdst: -1,
        gmtoff: 0,
        zone: std::ptr::null(),
    };
    // SAFETY: the pointer is valid for the call.
    let seconds = unsafe { mktime(&mut tm) };
    // -1 is also one second before 1970, which mktime gives as such.
    (seconds != -1 || (year, month, day, hour, minute, second) == (1969, 12, 31, 23, 59, 59))
        .then_some(seconds)
}

/// The moment `seconds` and `nanos` in UTC with `utc`, and otherwise at
/// the local zone's offset, which the C library gives; in UTC when it
/// cannot tell.
fn moment(seconds: i64, nanos: u32, utc: bool) -> Moment {
    let in_utc = Moment {
        seconds,
        nanos,
        offset: 0,
        zone: "UTC".into(),
    };
    if utc {
        return in_utc;
    }
    let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
    // SAFETY: both pointers are valid for the call.
    if unsafe { localtime_r(&seconds, tm.as_mut_ptr()) }.is_null() {
        return in_utc;
    }
    // SAFETY: `localtime_r` filled it in.
    let tm = unsafe { tm.assume_init() };
    let offset = i32::try_from(tm.gmtoff).unwrap_or(0);
    let zone = match tm.zone.is_null() {
        // SAFETY: the C library's zone names are C strings.
        false => unsafe { CStr::from_ptr(tm.zone) }
            .to_string_lossy()
            .into_owned(),
        true => crate::format::strftime(
            "%z",
            &Moment {
                offset,
                ..in_utc.clone()
            },
        ),
    };
    Moment {
        seconds,
        nanos,
        offset,
        zone,
    }
}

/// Sets the system time to `seconds` and `nanos` with the time service's
/// `SetUtc`, which refuses it with `ACCESS_DENIED` when no administrator
/// allowed it.
fn set(seconds: i64, nanos: u32) -> Result<(), Failure> {
    let failed = |why: String| (FAILURE, why);
    let utc = seconds
        .checked_mul(NANOS)
        .and_then(|n| n.checked_add(i64::from(nanos)))
        .ok_or_else(|| failed("the time is out of range".into()))?;
    let directory = Directory::new(std::os::mikros::directory().as_raw_handle())
        .ok_or_else(|| failed("cannot reach the service directory".into()))?;
    let clock = ClockClient::new(directory.connect(CLOCK).map_err(failed)?);
    match clock.set_utc(utc, FOREVER) {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(Status::AccessDenied)) => Err(failed("not authorized".into())),
        Ok(Err(status)) => Err(failed(format!("SetUtc failed: {status:?}"))),
        Err(error) => Err(failed(format!("SetUtc failed: {error:?}"))),
    }
}
