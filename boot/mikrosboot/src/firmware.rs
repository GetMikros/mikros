//! Checking firmware data before it is passed to the kernel.

use core::fmt;

/// Why an ACPI RSDP was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RsdpError {
    Signature,
    /// The revision is below 2.
    Revision(u8),
    /// The length field is too small or larger than the readable bytes.
    Length(u32),
    Checksum,
    ExtendedChecksum,
}

impl fmt::Display for RsdpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RsdpError::Signature => write!(f, "the ACPI RSDP has a wrong signature"),
            RsdpError::Revision(r) => write!(f, "the ACPI RSDP has revision {r}, below 2"),
            RsdpError::Length(l) => write!(f, "the ACPI RSDP has a bad length {l}"),
            RsdpError::Checksum => write!(f, "the ACPI RSDP checksum is wrong"),
            RsdpError::ExtendedChecksum => {
                write!(f, "the ACPI RSDP extended checksum is wrong")
            }
        }
    }
}

/// The size of an RSDP of revision 2.
pub const RSDP_SIZE: usize = 36;

fn sums_to_zero(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) == 0
}

/// Checks the RSDP at the start of `bytes`. `bytes` holds at least
/// `RSDP_SIZE` bytes, and more when the table's length field says so.
pub fn check_rsdp(bytes: &[u8]) -> Result<(), RsdpError> {
    if bytes.get(..8) != Some(&b"RSD PTR "[..]) || bytes.len() < RSDP_SIZE {
        return Err(RsdpError::Signature);
    }
    if bytes[15] < 2 {
        return Err(RsdpError::Revision(bytes[15]));
    }
    if !sums_to_zero(&bytes[..20]) {
        return Err(RsdpError::Checksum);
    }
    let length = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if (length as usize) < RSDP_SIZE || length as usize > bytes.len() {
        return Err(RsdpError::Length(length));
    }
    if !sums_to_zero(&bytes[..length as usize]) {
        return Err(RsdpError::ExtendedChecksum);
    }
    Ok(())
}

/// The `TimeZone` value of a time without a time zone.
pub const UNSPECIFIED_TIMEZONE: i16 = 0x07ff;

/// A firmware time, with the fields of `EFI_TIME` used here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Time {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// Minutes the local time is ahead of UTC, or `UNSPECIFIED_TIMEZONE`.
    pub time_zone: i16,
}

fn days_in_month(year: i64, month: u8) -> u8 {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to the given date of the proleptic Gregorian
/// calendar.
fn days_from_epoch(year: i64, month: u8, day: u8) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The seconds since 1970-01-01T00:00:00 UTC, or `None` when a field is out
/// of range or the time is before 1970.
pub fn unix_seconds(t: Time) -> Option<u64> {
    let year = i64::from(t.year);
    let date_ok = (1..=12).contains(&t.month)
        && (1..=days_in_month(year, t.month)).contains(&t.day)
        && (1900..=9999).contains(&t.year);
    let zone_ok = t.time_zone == UNSPECIFIED_TIMEZONE || (-1440..=1440).contains(&t.time_zone);
    if !date_ok || !zone_ok || t.hour > 23 || t.minute > 59 || t.second > 59 {
        return None;
    }
    let offset = if t.time_zone == UNSPECIFIED_TIMEZONE {
        0
    } else {
        i64::from(t.time_zone) * 60
    };
    let local = days_from_epoch(year, t.month, t.day) * 86_400
        + i64::from(t.hour) * 3600
        + i64::from(t.minute) * 60
        + i64::from(t.second);
    u64::try_from(local - offset).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn rsdp(revision: u8, length: u32) -> Vec<u8> {
        let mut t = Vec::from(*b"RSD PTR ");
        t.extend_from_slice(&[0; 7]);
        t.push(revision);
        t.extend_from_slice(&0x7fe_1000u32.to_le_bytes());
        t.extend_from_slice(&length.to_le_bytes());
        t.extend_from_slice(&0x7fe_2000u64.to_le_bytes());
        t.extend_from_slice(&[0; 4]);
        t.resize(length.max(36) as usize, 0);
        let sum = |b: &[u8]| b.iter().fold(0u8, |s, &x| s.wrapping_add(x));
        t[8] = 0u8.wrapping_sub(sum(&t[..20]));
        t[32] = 0u8.wrapping_sub(sum(&t));
        t
    }

    #[test]
    fn accepts_a_valid_rsdp() {
        assert_eq!(check_rsdp(&rsdp(2, 36)), Ok(()));
        assert_eq!(check_rsdp(&rsdp(3, 40)), Ok(()));
    }

    #[test]
    fn refuses_a_bad_rsdp() {
        let mut t = rsdp(2, 36);
        t[0] = b'X';
        assert_eq!(check_rsdp(&t), Err(RsdpError::Signature));
        assert_eq!(check_rsdp(&rsdp(0, 36)), Err(RsdpError::Revision(0)));
        let mut t = rsdp(2, 36);
        t[16] ^= 1;
        assert_eq!(check_rsdp(&t), Err(RsdpError::Checksum));
        let mut t = rsdp(2, 36);
        t[35] ^= 1;
        assert_eq!(check_rsdp(&t), Err(RsdpError::ExtendedChecksum));
        assert_eq!(check_rsdp(&rsdp(2, 20)), Err(RsdpError::Length(20)));
        let t = rsdp(2, 40);
        assert_eq!(check_rsdp(&t[..36]), Err(RsdpError::Length(40)));
    }

    fn time(year: u16, month: u8, day: u8, hms: (u8, u8, u8), time_zone: i16) -> Time {
        Time {
            year,
            month,
            day,
            hour: hms.0,
            minute: hms.1,
            second: hms.2,
            time_zone,
        }
    }

    #[test]
    fn converts_to_unix_seconds() {
        let utc = UNSPECIFIED_TIMEZONE;
        assert_eq!(unix_seconds(time(1970, 1, 1, (0, 0, 0), utc)), Some(0));
        assert_eq!(
            unix_seconds(time(2000, 2, 29, (12, 34, 56), utc)),
            Some(951_827_696)
        );
        assert_eq!(
            unix_seconds(time(2026, 11, 1, (23, 59, 59), 0)),
            Some(1_793_577_599)
        );
        assert_eq!(
            unix_seconds(time(2026, 11, 2, (1, 0, 0), 60)),
            Some(1_793_577_600)
        );
    }

    #[test]
    fn refuses_invalid_times() {
        let utc = UNSPECIFIED_TIMEZONE;
        assert_eq!(unix_seconds(time(2023, 2, 29, (0, 0, 0), utc)), None);
        assert_eq!(unix_seconds(time(2024, 13, 1, (0, 0, 0), utc)), None);
        assert_eq!(unix_seconds(time(2024, 1, 1, (24, 0, 0), utc)), None);
        assert_eq!(unix_seconds(time(2024, 1, 1, (0, 0, 0), 2000)), None);
        assert_eq!(unix_seconds(time(1969, 12, 31, (23, 59, 59), utc)), None);
        assert_eq!(unix_seconds(time(1970, 1, 1, (0, 0, 0), 60)), None);
    }
}
