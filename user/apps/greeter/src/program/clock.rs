//! The local date and time. The C library turns the UTC time into local
//! time with the zone that the time service names, read from the zone files
//! of `/system/share/zoneinfo`; neither needs a permission.

use crate::model::Moment;
use std::ffi::{c_char, c_int, c_long};
use std::time::{SystemTime, UNIX_EPOCH};

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
    fn tzset();
}

/// Reads the time zone again. The C library asks the time service for it
/// once, so a greeter that started before the service had the zone, or a
/// zone that was set since, shows only after this.
pub fn refresh() {
    // SAFETY: `tzset` has no arguments and the program's threads do not
    // change the environment.
    unsafe { tzset() }
}

/// The local time now, or None when the clock or the zone cannot say.
pub fn now() -> Option<Moment> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let seconds = i64::try_from(seconds.as_secs()).ok()?;
    let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
    // SAFETY: both pointers are valid for the call.
    if unsafe { localtime_r(&seconds, tm.as_mut_ptr()) }.is_null() {
        return None;
    }
    // SAFETY: `localtime_r` filled it in.
    let tm = unsafe { tm.assume_init() };
    Some(Moment {
        weekday: u8::try_from(tm.wday).ok()?,
        month: u8::try_from(tm.mon + 1).ok()?,
        day: u8::try_from(tm.mday).ok()?,
        hour: u8::try_from(tm.hour).ok()?,
        minute: u8::try_from(tm.min).ok()?,
    })
}
