//! Times as the file commands show them (MIKSPEC-0098.R33): local time to
//! the minute in text, RFC 3339 in UTC in JSON.

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
}

/// The seconds since 1970 of `time`, negative before.
pub fn seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// `seconds` since 1970 in local time, as `YYYY-MM-DD HH:MM`.
pub fn local(seconds: i64) -> String {
    let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
    // SAFETY: both pointers are valid for the call.
    if unsafe { localtime_r(&seconds, tm.as_mut_ptr()) }.is_null() {
        return utc(seconds)[..16].replace('T', " ");
    }
    // SAFETY: `localtime_r` filled it in.
    let tm = unsafe { tm.assume_init() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.year + 1900,
        tm.mon + 1,
        tm.mday,
        tm.hour,
        tm.min
    )
}

/// `seconds` since 1970 in RFC 3339, in UTC.
pub fn utc(seconds: i64) -> String {
    let (year, month, day) = mikros_tz::civil(seconds.div_euclid(86_400));
    let of_day = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_utc_and_local_times() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_794_733_687), "2026-11-15T09:08:07Z");
        assert_eq!(utc(-1), "1969-12-31T23:59:59Z");
        let shown = local(1_794_733_687);
        assert_eq!(shown.len(), 16);
        assert_eq!(&shown[4..5], "-");
        assert_eq!(&shown[10..11], " ");
    }
}
