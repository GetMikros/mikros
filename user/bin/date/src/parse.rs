//! The dates that `-d` and `-s` take: `now`, `@` and seconds since 1970,
//! or a date `YYYY-MM-DD`, a time `HH:MM[:SS[.fraction]]` after it or
//! alone, separated by `T` or spaces, and a zone after them: `Z`, `UTC`,
//! `GMT`, or an offset `+HH`, `+HHMM` or `+HH:MM`, or with `-`.

/// What a date names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Given {
    Now,
    /// Seconds since 1970, and nanoseconds after them.
    Epoch(i64, u32),
    /// A local time: today when it has no date, and in the zone of the
    /// run when it has no offset.
    Civil {
        date: Option<(i64, u32, u32)>,
        /// Hours, minutes, seconds and nanoseconds.
        time: (u32, u32, u32, u32),
        /// Seconds east of UTC.
        offset: Option<i32>,
    },
}

/// The date of `text`, or `None` when it is none of the forms.
pub fn given(text: &str) -> Option<Given> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("now") {
        return Some(Given::Now);
    }
    if let Some(epoch) = text.strip_prefix('@') {
        return epoch_of(epoch);
    }
    let (rest, offset) = zone(text)?;
    let (date, time) = match rest.split_once(['T', 't', ' ']) {
        Some((date, time)) => (Some(date_of(date)?), Some(time_of(time.trim_start())?)),
        None if rest.contains(':') => (None, Some(time_of(rest)?)),
        None => (Some(date_of(rest)?), None),
    };
    Some(Given::Civil {
        date,
        time: time.unwrap_or((0, 0, 0, 0)),
        offset,
    })
}

/// Seconds, perhaps negative, and a fraction.
fn epoch_of(text: &str) -> Option<Given> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let (negative, digits) = match whole.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, whole),
    };
    if digits.is_empty() || !all_digits(digits) {
        return None;
    }
    let seconds: i64 = digits.parse().ok()?;
    let nanos = nanos_of(fraction)?;
    Some(match (negative, nanos) {
        (false, _) => Given::Epoch(seconds, nanos),
        (true, 0) => Given::Epoch(-seconds, 0),
        (true, n) => Given::Epoch(-seconds - 1, 1_000_000_000 - n),
    })
}

fn all_digits(text: &str) -> bool {
    text.bytes().all(|b| b.is_ascii_digit())
}

/// The nanoseconds of the digits after a decimal point.
fn nanos_of(fraction: &str) -> Option<u32> {
    if !all_digits(fraction) {
        return None;
    }
    let mut nanos = 0;
    let mut scale = 1_000_000_000;
    for b in fraction.bytes().take(9) {
        scale /= 10;
        nanos += u32::from(b - b'0') * scale;
    }
    Some(nanos)
}

/// `text` without the zone at its end, and the zone's offset, if it has
/// one.
fn zone(text: &str) -> Option<(&str, Option<i32>)> {
    for name in ["Z", "UTC", "GMT"] {
        if text.len() > name.len() && text.is_char_boundary(text.len() - name.len()) {
            let (rest, end) = text.split_at(text.len() - name.len());
            // A `Z` takes no space before it; the names may.
            if end.eq_ignore_ascii_case(name)
                && (name == "Z"
                    || rest.ends_with([' ', 'T', 't'])
                    || rest.ends_with(|c: char| c.is_ascii_digit()))
            {
                return Some((rest.trim_end(), Some(0)));
            }
        }
    }
    // An offset follows a time, after its last colon or digit.
    let Some(at) = text.rfind(['+', '-']) else {
        return Some((text, None));
    };
    let (rest, sign_and_offset) = text.split_at(at);
    if !rest.contains(':') {
        return Some((text, None));
    }
    let sign = if sign_and_offset.starts_with('-') {
        -1
    } else {
        1
    };
    let offset = &sign_and_offset[1..];
    let (hours, minutes) = match (offset.len(), offset.split_once(':')) {
        (_, Some((h, m))) if h.len() == 2 && m.len() == 2 => (h, m),
        (2, None) => (offset, "00"),
        (4, None) => offset.split_at(2),
        _ => return None,
    };
    if !all_digits(hours) || !all_digits(minutes) {
        return None;
    }
    let (hours, minutes): (i32, i32) = (hours.parse().ok()?, minutes.parse().ok()?);
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some((rest.trim_end(), Some(sign * (hours * 3600 + minutes * 60))))
}

/// `YYYY-MM-DD`, of a day that exists.
fn date_of(text: &str) -> Option<(i64, u32, u32)> {
    let mut parts = text.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    if !all_digits(year) || !all_digits(month) || !all_digits(day) {
        return None;
    }
    let (year, month, day): (i64, u32, u32) =
        (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
    let length = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if mikros_tz::is_leap(year) => 29,
        2 => 28,
        _ => return None,
    };
    (1..=length).contains(&day).then_some((year, month, day))
}

/// `HH:MM`, `HH:MM:SS` or `HH:MM:SS.fraction`.
fn time_of(text: &str) -> Option<(u32, u32, u32, u32)> {
    let (clock, fraction) = text.split_once('.').unwrap_or((text, ""));
    let parts: Vec<&str> = clock.split(':').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.len() != 2 || !all_digits(p)) {
        return None;
    }
    if parts.len() == 2 && !fraction.is_empty() {
        return None;
    }
    let number = |i: usize| parts.get(i).map_or(Some(0), |p| p.parse::<u32>().ok());
    let (hour, minute, second) = (number(0)?, number(1)?, number(2)?);
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour, minute, second, nanos_of(fraction)?))
}

/// The seconds since 1970 of the local time `date` and `time` at
/// `offset` seconds east of UTC.
pub fn utc_of(date: (i64, u32, u32), time: (u32, u32, u32), offset: i32) -> i64 {
    let (year, month, day) = date;
    let (hour, minute, second) = time;
    mikros_tz::days(year, month, day) * 86_400 + i64::from(hour * 3600 + minute * 60 + second)
        - i64::from(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn civil(
        date: Option<(i64, u32, u32)>,
        time: (u32, u32, u32, u32),
        offset: Option<i32>,
    ) -> Option<Given> {
        Some(Given::Civil { date, time, offset })
    }

    #[test]
    fn reads_now_and_seconds() {
        assert_eq!(given(" now "), Some(Given::Now));
        assert_eq!(given("@0"), Some(Given::Epoch(0, 0)));
        assert_eq!(
            given("@1794733687.5"),
            Some(Given::Epoch(1_794_733_687, 500_000_000))
        );
        assert_eq!(given("@-1.25"), Some(Given::Epoch(-2, 750_000_000)));
        assert_eq!(given("@-3"), Some(Given::Epoch(-3, 0)));
        for bad in ["@", "@x", "@1.x", "@--1"] {
            assert_eq!(given(bad), None, "{bad}");
        }
    }

    #[test]
    fn reads_dates_and_times() {
        let day = Some((2026, 11, 15));
        assert_eq!(given("2026-11-15"), civil(day, (0, 0, 0, 0), None));
        assert_eq!(given("2026-11-15 10:08"), civil(day, (10, 8, 0, 0), None));
        assert_eq!(
            given("2026-11-15T10:08:07"),
            civil(day, (10, 8, 7, 0), None)
        );
        assert_eq!(
            given("2026-11-15 10:08:07.25"),
            civil(day, (10, 8, 7, 250_000_000), None)
        );
        assert_eq!(given("23:59"), civil(None, (23, 59, 0, 0), None));
        assert_eq!(
            given("2024-02-29"),
            civil(Some((2024, 2, 29)), (0, 0, 0, 0), None)
        );
    }

    #[test]
    fn reads_zones() {
        let day = Some((2026, 11, 15));
        let time = (10, 8, 7, 0);
        assert_eq!(given("2026-11-15T10:08:07Z"), civil(day, time, Some(0)));
        assert_eq!(given("2026-11-15 10:08:07 UTC"), civil(day, time, Some(0)));
        assert_eq!(
            given("2026-11-15 10:08:07+01:00"),
            civil(day, time, Some(3600))
        );
        assert_eq!(
            given("2026-11-15 10:08:07 -0530"),
            civil(day, time, Some(-19_800))
        );
        assert_eq!(
            given("2026-11-15 10:08:07+02"),
            civil(day, time, Some(7200))
        );
        assert_eq!(given("10:08 utc"), civil(None, (10, 8, 0, 0), Some(0)));
    }

    #[test]
    fn refuses_what_is_no_date() {
        for bad in [
            "",
            "tomorrow",
            "2026-13-01",
            "2026-02-29",
            "2026-11-31",
            "26-11-15",
            "2026-11-15 25:00",
            "2026-11-15 10:60",
            "2026-11-15 10:08:07+1",
            "2026-11-15 10:08:07+24:00",
            "10",
            "10:08.5",
            "2026-11-15 10:08 x",
        ] {
            assert_eq!(given(bad), None, "{bad}");
        }
    }

    #[test]
    fn gives_seconds_since_1970() {
        assert_eq!(utc_of((1970, 1, 1), (0, 0, 0), 0), 0);
        assert_eq!(utc_of((2026, 11, 15), (10, 8, 7), 3600), 1_794_733_687);
        assert_eq!(utc_of((1969, 12, 31), (23, 59, 59), 0), -1);
    }
}
