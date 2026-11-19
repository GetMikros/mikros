//! uptime on Mikros: how long the system has run is the monotonic clock
//! (MIKSPEC-0008.R21), which the C library's `CLOCK_MONOTONIC` reads,
//! and local times come from the C library's `localtime_r`, in the
//! system's time zone unless `TZ` names another (MIKSPEC-0033.R42).

use crate::since::{self, COMMAND, Civil, Show};
use mikros_cli::{FAILURE, INVALID, Paint, SUCCESS, complain, start, time};
use std::ffi::{c_char, c_int, c_long};
use std::io::Write;
use std::process::ExitCode;
use std::time::SystemTime;

const CLOCK_MONOTONIC: c_int = 1;

#[repr(C)]
struct Timespec {
    seconds: i64,
    nanoseconds: c_long,
}

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
    fn clock_gettime(clock: c_int, time: *mut Timespec) -> c_int;
    fn localtime_r(time: *const i64, tm: *mut Tm) -> *mut Tm;
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let show = match since::show(&run) {
        Ok(show) => show,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let mut now = Timespec {
        seconds: 0,
        nanoseconds: 0,
    };
    // SAFETY: the pointer is valid for the call.
    if unsafe { clock_gettime(CLOCK_MONOTONIC, &mut now) } != 0 {
        let e = std::io::Error::last_os_error();
        complain(
            &COMMAND,
            &run,
            &format!("cannot read the monotonic clock: {e}"),
        );
        return ExitCode::from(FAILURE);
    }
    let up = u64::try_from(now.seconds).unwrap_or(0);
    let utc = time::seconds(SystemTime::now());
    let paint = match show {
        Show::Json => Paint::never(),
        _ => Paint::stdout(&run),
    };
    let since = utc.saturating_sub(i64::try_from(up).unwrap_or(i64::MAX));
    let text = match show {
        Show::Line => since::line(local(utc), up, paint),
        Show::Pretty => since::pretty(up, paint),
        Show::Since => since::started(local(since), paint),
        Show::Json => since::json(utc, up),
    };
    match std::io::stdout().write_all(text.as_bytes()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(_) => ExitCode::from(FAILURE),
    }
}

/// `seconds` since 1970 in local time, or in UTC when the C library
/// cannot tell.
fn local(seconds: i64) -> Civil {
    let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
    // SAFETY: both pointers are valid for the call.
    if unsafe { localtime_r(&seconds, tm.as_mut_ptr()) }.is_null() {
        // As `YYYY-MM-DDTHH:MM:SSZ`.
        let utc = time::utc(seconds);
        let field = |at: usize, len: usize| utc.get(at..at + len).and_then(|f| f.parse().ok());
        return Civil {
            year: field(0, 4).unwrap_or(1970) as i32,
            month: field(5, 2).unwrap_or(1),
            day: field(8, 2).unwrap_or(1),
            hour: field(11, 2).unwrap_or(0),
            minute: field(14, 2).unwrap_or(0),
            second: field(17, 2).unwrap_or(0),
        };
    }
    // SAFETY: `localtime_r` filled it in.
    let tm = unsafe { tm.assume_init() };
    Civil {
        year: tm.year + 1900,
        month: (tm.mon + 1) as u32,
        day: tm.mday as u32,
        hour: tm.hour as u32,
        minute: tm.min as u32,
        second: tm.sec as u32,
    }
}
