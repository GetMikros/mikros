//! Time: UTC from the time service's time page, and FAT's local
//! timestamps in UTC. The system time zone is not read yet, so local
//! time is taken as UTC.

use core::sync::atomic::{Ordering, fence};
use mikros_fat::Stamp;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::interfaces::mikros::time::{Clock, ClockClient};
use mikros_rt::sys;

const CLOCK_PROTOCOL: &str = "mikros.time.Clock";
const FOREVER: i64 = i64::MAX;
const PERM_READ: u32 = 1 << 0;
const NS: i64 = 1_000_000_000;
const DAY: i64 = 86_400;

/// UTC, from the time page once the time service gives it.
pub struct Utc {
    page: Option<u64>,
}

impl Utc {
    /// UTC through the time service in `directory`, when it runs.
    pub fn new(directory: &DirectoryClient, process: sys::Handle) -> Utc {
        Utc {
            page: time_page(directory, process),
        }
    }

    /// UTC now, in nanoseconds since 1970, or 0 while it is not known.
    pub fn now(&self) -> i64 {
        let Some(page) = self.page else {
            return 0;
        };
        let read = |at: u64| {
            // SAFETY: the page is mapped read-only for as long as the
            // process runs.
            unsafe { core::ptr::read_volatile(at as *const u64) }
        };
        let offset = loop {
            let before = read(page) as u32;
            fence(Ordering::Acquire);
            let offset = read(page + 8) as i64;
            fence(Ordering::Acquire);
            if before % 2 == 0 && read(page) as u32 == before {
                break offset;
            }
            core::hint::spin_loop();
        };
        let mut monotonic = 0;
        // SAFETY: the out pointer is valid for the call.
        unsafe { sys::clock_get_monotonic(&mut monotonic) };
        offset.saturating_add(monotonic)
    }
}

/// Maps the time page of the time service in `directory`.
fn time_page(directory: &DirectoryClient, process: sys::Handle) -> Option<u64> {
    let (client, server) = endpoints::<Clock>().ok()?;
    match directory.connect(CLOCK_PROTOCOL.into(), None, server.into_handle(), FOREVER) {
        Ok(Ok(_)) => {}
        _ => return None,
    }
    let clock = ClockClient::new(ClientEnd::new(client.into_handle()));
    let page = clock.get_time_page(FOREVER).ok()?.page;
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let status = unsafe { sys::vm_map(process, PERM_READ, 0, page.raw(), 0, 4096, &mut at) };
    (status == mikros_rt::OK).then_some(at)
}

/// Days since 1970-01-01 of a civil date.
fn days(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of `days` since 1970-01-01.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// The UTC time, in nanoseconds since 1970, of a FAT stamp.
pub fn to_utc(stamp: Stamp) -> i64 {
    let (year, month, day, hour, minute, second, ms) = stamp.parts();
    let date = days(i64::from(year), i64::from(month), i64::from(day));
    let seconds = date * DAY + i64::from(hour * 3600 + minute * 60 + second);
    seconds * NS + i64::from(ms) * 1_000_000
}

/// The FAT stamp of a UTC time, clamped to what FAT can hold.
pub fn to_stamp(utc: i64) -> Stamp {
    let seconds = utc.div_euclid(NS);
    let ms = utc.rem_euclid(NS) / 1_000_000;
    let (year, month, day) = civil(seconds.div_euclid(DAY));
    let time = seconds.rem_euclid(DAY);
    if year < 1980 {
        return Stamp::new(1980, 1, 1, 0, 0, 0, 0);
    }
    if year > 2107 {
        return Stamp::new(2107, 12, 31, 23, 59, 59, 990);
    }
    let (h, m, s) = (time / 3600, time / 60 % 60, time % 60);
    Stamp::new(
        year as u32,
        month as u32,
        day as u32,
        h as u32,
        m as u32,
        s as u32,
        ms as u32,
    )
}
