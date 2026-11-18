//! touch on Mikros: it creates files through the standard library and
//! sets their times with the C library's `utimensat` (MIKSPEC-0098.R33),
//! reading local times with `mktime`.

use crate::stamps::{self, COMMAND, Civil, Today, When, Which, Zone};
use mikros_cli::{FAILURE, INVALID, Run, SUCCESS, complain, start};
use std::ffi::{CString, c_char, c_int, c_long};
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

const AT_FDCWD: c_int = -100;
const UTIME_NOW: i64 = 0x3fff_ffff;
const UTIME_OMIT: i64 = 0x3fff_fffe;

/// The C library's `struct timespec`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Timespec {
    sec: i64,
    nsec: i64,
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
    fn utimensat(dirfd: c_int, path: *const c_char, times: *const Timespec, flags: c_int) -> c_int;
    fn localtime_r(time: *const i64, tm: *mut Tm) -> *mut Tm;
    fn mktime(tm: *mut Tm) -> i64;
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let times = match times(&run) {
        Ok(times) => times,
        Err((why, status)) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(status);
        }
    };
    let mut failed = false;
    for operand in &run.operands {
        if let Err(why) = touch(Path::new(operand), &times, run.has("-c")) {
            complain(&COMMAND, &run, &why);
            failed = true;
        }
    }
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

/// The access and modified times to set, from the options, or why there
/// are none, with the exit status.
fn times(run: &Run) -> Result<[Timespec; 2], (String, u8)> {
    let given = ["-d", "-t", "-r"].iter().filter(|o| run.has(o)).count();
    if given > 1 {
        return Err(("give only one of -d, -t and -r".into(), INVALID));
    }
    let now = Timespec {
        sec: 0,
        nsec: UTIME_NOW,
    };
    let (access, modified) = if let Some(text) = run.value("-d") {
        let when = stamps::date(text, today());
        let when = when.ok_or((format!("invalid date {text}"), INVALID))?;
        let time = resolve(when).ok_or((format!("invalid date {text}"), INVALID))?;
        (time, time)
    } else if let Some(text) = run.value("-t") {
        let when = stamps::stamp(text, today().year);
        let when = when.ok_or((format!("invalid stamp {text}"), INVALID))?;
        let time = resolve(when).ok_or((format!("invalid stamp {text}"), INVALID))?;
        (time, time)
    } else if let Some(file) = run.value("-r") {
        let m = fs::metadata(file).map_err(|e| (format!("cannot access {file}: {e}"), FAILURE))?;
        let access = Timespec {
            sec: m.atime(),
            nsec: m.atime_nsec(),
        };
        let modified = Timespec {
            sec: m.mtime(),
            nsec: m.mtime_nsec(),
        };
        (access, modified)
    } else {
        (now, now)
    };
    let which = Which::of(run);
    let omit = Timespec {
        sec: 0,
        nsec: UTIME_OMIT,
    };
    Ok([
        if which.access { access } else { omit },
        if which.modified { modified } else { omit },
    ])
}

/// The seconds since 1970 and nanoseconds of `when`, if it is a time the
/// C library can give.
fn resolve(when: When) -> Option<Timespec> {
    let (sec, nsec) = match when {
        When::Now(offset) => {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
            let sec = i64::try_from(now.as_secs()).ok()?.checked_add(offset)?;
            (sec, now.subsec_nanos())
        }
        When::Seconds(sec, nsec) => (sec, nsec),
        When::Civil(civil, nsec, Zone::Offset(east)) => (civil.utc() - east, nsec),
        When::Civil(civil, nsec, Zone::Local) => (local(civil)?, nsec),
    };
    Some(Timespec {
        sec,
        nsec: i64::from(nsec),
    })
}

/// The seconds since 1970 of `civil` in local time.
fn local(civil: Civil) -> Option<i64> {
    let mut tm = Tm {
        sec: c_int::try_from(civil.second).ok()?,
        min: c_int::try_from(civil.minute).ok()?,
        hour: c_int::try_from(civil.hour).ok()?,
        mday: c_int::try_from(civil.day).ok()?,
        mon: c_int::try_from(civil.month).ok()? - 1,
        year: c_int::try_from(civil.year - 1900).ok()?,
        wday: 0,
        yday: 0,
        // Whether summer time applies, the C library finds out.
        isdst: -1,
        gmtoff: 0,
        zone: std::ptr::null(),
    };
    // SAFETY: the pointer is valid for the call.
    let seconds = unsafe { mktime(&mut tm) };
    // -1 is also one second before 1970, which then reads 59 seconds.
    match seconds == -1 && civil.second != 59 {
        true => None,
        false => Some(seconds),
    }
}

/// Today's local date.
fn today() -> Today {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
    // SAFETY: both pointers are valid for the call.
    if unsafe { localtime_r(&now, tm.as_mut_ptr()) }.is_null() {
        // UTC's date, as `YYYY-MM-DD...`.
        let utc = mikros_cli::time::utc(now);
        return Today {
            year: utc[..4].parse().unwrap_or(1970),
            month: utc[5..7].parse().unwrap_or(1),
            day: utc[8..10].parse().unwrap_or(1),
        };
    }
    // SAFETY: `localtime_r` filled it in.
    let tm = unsafe { tm.assume_init() };
    Today {
        year: i64::from(tm.year) + 1900,
        month: u32::try_from(tm.mon + 1).unwrap_or(1),
        day: u32::try_from(tm.mday).unwrap_or(1),
    }
}

/// Creates `path` unless it exists or `no_create`, and sets its `times`,
/// following a link.
fn touch(path: &Path, times: &[Timespec; 2], no_create: bool) -> Result<(), String> {
    let shown = path.display();
    match fs::metadata(path) {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {
            if no_create {
                return Ok(());
            }
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
                .map_err(|e| format!("cannot touch {shown}: {e}"))?;
        }
        Err(e) => return Err(format!("cannot touch {shown}: {e}")),
    }
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("cannot touch {shown}: the path holds a NUL"))?;
    // SAFETY: the path is NUL-terminated and `times` holds two.
    let set = unsafe { utimensat(AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
    match set {
        0 => Ok(()),
        _ => Err(format!(
            "cannot set the times of {shown}: {}",
            io::Error::last_os_error()
        )),
    }
}
