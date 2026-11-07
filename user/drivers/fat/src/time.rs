//! FAT's local timestamps in UTC. The system time zone is not read
//! yet, so local time is taken as UTC.

use mikros_fat::Stamp;

const NS: i64 = 1_000_000_000;
const DAY: i64 = 86_400;

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

/// The UTC time, in nanoseconds since 1970, of a FAT stamp.
pub fn to_utc(stamp: Stamp) -> i64 {
    let (year, month, day, hour, minute, second, ms) = stamp.parts();
    let date = days(i64::from(year), i64::from(month), i64::from(day));
    let seconds = date * DAY + i64::from(hour * 3600 + minute * 60 + second);
    seconds * NS + i64::from(ms) * 1_000_000
}
