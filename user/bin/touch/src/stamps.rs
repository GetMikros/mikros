//! What touch takes, and the times that `-d` and `-t` give
//! (MIKSPEC-0098.R38).
//!
//! `-t` takes the stamp of POSIX touch, `[[CC]YY]MMDDhhmm[.ss]` in local
//! time. `-d` takes these forms of GNU touch: `now`, `today`, `yesterday`
//! and `tomorrow`; `@<seconds>`; and a date `YYYY-MM-DD`, a time
//! `hh:mm[:ss[.frac]]`, or both, apart by a space or `T`, in local time
//! or followed by a zone, `Z`, `UTC`, `GMT`, `+hh`, `+hhmm` or `+hh:mm`
//! (or with `-`). A time alone is on today's date.

use mikros_cli::{Kind, Operand, Opt, Run, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::some(
        "file",
        Kind::File,
        "the files to create or set the times of",
    )],
    options: &[
        Opt::short('a', "set only the access time"),
        Opt::short('m', "set only the modified time"),
        Opt::both("no-create", 'c', "create no file that does not exist"),
        Opt::both(
            "date",
            'd',
            "use this time: now, yesterday, @seconds or YYYY-MM-DD hh:mm[:ss] [zone]",
        )
        .takes("date", Kind::Text),
        Opt::short('t', "use this local time: [[CC]YY]MMDDhhmm[.ss]").takes("stamp", Kind::Text),
        Opt::both("reference", 'r', "use the times of this file").takes("file", Kind::File),
    ],
    ..Spec::new(
        "touch",
        "Creates the files that do not exist, and sets the access and modified\n\
         times of each to now, or to the time given.",
    )
};

const DAY: i64 = 86_400;

/// Which times a run sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Which {
    pub access: bool,
    pub modified: bool,
}

impl Which {
    /// Both, unless `-a` or `-m` asks for only one.
    pub fn of(run: &Run) -> Which {
        let (a, m) = (run.has("-a"), run.has("-m"));
        Which {
            access: a || !m,
            modified: m || !a,
        }
    }
}

/// A date and time of day, in a zone that `When` gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl Civil {
    /// The seconds since 1970 of this time read as UTC.
    pub fn utc(self) -> i64 {
        days(self.year, self.month, self.day) * DAY
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }
}

/// The zone of a civil time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Local,
    /// Seconds east of UTC.
    Offset(i64),
}

/// A time given with `-d` or `-t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    /// Now, moved by these seconds.
    Now(i64),
    /// Seconds since 1970, and nanoseconds.
    Seconds(i64, u32),
    /// A civil time in a zone, and nanoseconds.
    Civil(Civil, u32, Zone),
}

/// Today's local date, for the forms that leave it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Today {
    pub year: i64,
    pub month: u32,
    pub day: u32,
}

/// The days from 1970-01-01 to the date.
pub fn days(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year.rem_euclid(400);
    let month = i64::from(month);
    let of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let of_era_days = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * 146_097 + of_era_days - 719_468
}

fn month_days(year: i64, month: u32) -> u32 {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The civil time, if every field is in range; a second of 60 only with
/// `leap`.
fn civil(date: (i64, u32, u32), time: (u32, u32, u32), leap: bool) -> Option<Civil> {
    let (year, month, day) = date;
    let (hour, minute, second) = time;
    let ok = (1..=12).contains(&month)
        && day >= 1
        && day <= month_days(year, month)
        && hour < 24
        && minute < 60
        && (second < 60 || (leap && second == 60));
    ok.then_some(Civil {
        year,
        month,
        day,
        hour,
        minute,
        second,
    })
}

/// The number of the two ASCII digits at `at` of `text`.
fn two(text: &str, at: usize) -> Option<u32> {
    let pair = text.get(at..at + 2)?;
    match pair.bytes().all(|b| b.is_ascii_digit()) {
        true => pair.parse().ok(),
        false => None,
    }
}

/// The stamp of `-t`, `[[CC]YY]MMDDhhmm[.ss]`, in local time, in
/// `this_year` when it has no year. A two-digit year from 69 is in the
/// 1900s, below in the 2000s.
pub fn stamp(text: &str, this_year: i64) -> Option<When> {
    let (main, second) = match text.split_once('.') {
        Some((main, seconds)) if seconds.len() == 2 => (main, two(seconds, 0)?),
        Some(_) => return None,
        None => (text, 0),
    };
    if !main.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (year, rest) = match main.len() {
        8 => (this_year, main),
        10 => {
            let yy = i64::from(two(main, 0)?);
            (if yy >= 69 { 1900 + yy } else { 2000 + yy }, &main[2..])
        }
        12 => (main[..4].parse().ok()?, &main[4..]),
        _ => return None,
    };
    let date = (year, two(rest, 0)?, two(rest, 2)?);
    let time = (two(rest, 4)?, two(rest, 6)?, second);
    Some(When::Civil(civil(date, time, true)?, 0, Zone::Local))
}

/// The time of `-d`, in one of the forms that the module names.
pub fn date(text: &str, today: Today) -> Option<When> {
    let text = text.trim();
    match text.to_ascii_lowercase().as_str() {
        "now" | "today" => return Some(When::Now(0)),
        "yesterday" => return Some(When::Now(-DAY)),
        "tomorrow" => return Some(When::Now(DAY)),
        _ => {}
    }
    if let Some(seconds) = text.strip_prefix('@') {
        let (whole, nanos) = split_fraction(seconds)?;
        let negative = whole.starts_with('-');
        let digits = whole.strip_prefix(['-', '+']).unwrap_or(whole);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let value: i64 = digits.parse().ok()?;
        return Some(match (negative, nanos) {
            (false, _) => When::Seconds(value, nanos),
            (true, 0) => When::Seconds(-value, 0),
            // -1.25 is 1.75 before -1.
            (true, _) => When::Seconds(-value - 1, 1_000_000_000 - nanos),
        });
    }
    let (date, rest) = match leading_date(text) {
        Some((date, rest)) => (Some(date), rest),
        None => (None, text),
    };
    let rest = match date {
        Some(_) => rest.strip_prefix(['T', 't']).unwrap_or(rest).trim_start(),
        None => rest,
    };
    let (time, nanos, rest) = match leading_time(rest) {
        Some((time, nanos, rest)) => (Some(time), nanos, rest),
        None => (None, 0, rest),
    };
    let date = match (date, time) {
        (None, None) => return None,
        (Some(date), _) => date,
        (None, Some(_)) => (today.year, today.month, today.day),
    };
    let zone = zone(rest.trim())?;
    let civil = civil(date, time.unwrap_or((0, 0, 0)), false)?;
    Some(When::Civil(civil, nanos, zone))
}

/// `YYYY-MM-DD` at the start of `text`, and what follows.
fn leading_date(text: &str) -> Option<((i64, u32, u32), &str)> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits < 4 || text.as_bytes().get(digits) != Some(&b'-') {
        return None;
    }
    let year: i64 = text[..digits].parse().ok()?;
    let rest = &text[digits + 1..];
    let month = two(rest, 0)?;
    if rest.as_bytes().get(2) != Some(&b'-') {
        return None;
    }
    let day = two(rest, 3)?;
    let after = &rest[5..];
    match after.bytes().next() {
        None | Some(b' ' | b'T' | b't') => Some(((year, month, day), after)),
        _ => None,
    }
}

/// `hh:mm[:ss[.frac]]` at the start of `text`, its nanoseconds, and what
/// follows.
fn leading_time(text: &str) -> Option<((u32, u32, u32), u32, &str)> {
    let hour = two(text, 0)?;
    if text.as_bytes().get(2) != Some(&b':') {
        return None;
    }
    let minute = two(text, 3)?;
    let mut rest = &text[5..];
    let mut second = 0;
    let mut nanos = 0;
    if let Some(after) = rest.strip_prefix(':') {
        second = two(after, 0)?;
        rest = &after[2..];
        if let Some(fraction) = rest.strip_prefix('.') {
            let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
            let (_, n) = split_fraction(&rest[..digits + 1])?;
            nanos = n;
            rest = &fraction[digits..];
        }
    }
    Some(((hour, minute, second), nanos, rest))
}

/// `text` split at its `.`, with the digits after it as nanoseconds, of
/// which those beyond nine are dropped.
fn split_fraction(text: &str) -> Option<(&str, u32)> {
    let Some((whole, fraction)) = text.split_once('.') else {
        return Some((text, 0));
    };
    if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut nanos = 0;
    for i in 0..9 {
        let digit = fraction
            .as_bytes()
            .get(i)
            .map_or(0, |b| u32::from(b - b'0'));
        nanos = nanos * 10 + digit;
    }
    Some((whole, nanos))
}

/// The zone that `text` names, local when it is empty.
fn zone(text: &str) -> Option<Zone> {
    match text {
        "" => return Some(Zone::Local),
        "Z" | "z" | "UTC" | "utc" | "GMT" | "gmt" => return Some(Zone::Offset(0)),
        _ => {}
    }
    let sign = match text.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits = text[1..].replacen(':', "", 1);
    let (hours, minutes) = match digits.len() {
        2 => (two(&digits, 0)?, 0),
        4 if text.len() == 5 || text.as_bytes()[3] == b':' => (two(&digits, 0)?, two(&digits, 2)?),
        _ => return None,
    };
    if hours > 14 || minutes > 59 {
        return None;
    }
    Some(Zone::Offset(sign * i64::from(hours * 3600 + minutes * 60)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    const TODAY: Today = Today {
        year: 2026,
        month: 11,
        day: 18,
    };

    fn at(date: (i64, u32, u32), time: (u32, u32, u32)) -> Civil {
        civil(date, time, true).unwrap()
    }

    fn local(date: (i64, u32, u32), time: (u32, u32, u32)) -> Option<When> {
        Some(When::Civil(at(date, time), 0, Zone::Local))
    }

    #[test]
    fn counts_days_and_seconds_as_utc() {
        assert_eq!(days(1970, 1, 1), 0);
        assert_eq!(days(1969, 12, 31), -1);
        assert_eq!(days(2000, 3, 1), 11_017);
        assert_eq!(at((2026, 11, 15), (9, 8, 7)).utc(), 1_794_733_687);
        assert_eq!(at((1900, 1, 1), (0, 0, 0)).utc(), -2_208_988_800);
    }

    #[test]
    fn reads_stamps() {
        assert_eq!(stamp("11180930", 2026), local((2026, 11, 18), (9, 30, 0)));
        assert_eq!(stamp("6901010000", 2026), local((1969, 1, 1), (0, 0, 0)));
        assert_eq!(stamp("6801010000.05", 2026), local((2068, 1, 1), (0, 0, 5)));
        assert_eq!(
            stamp("202402291200.60", 2026),
            local((2024, 2, 29), (12, 0, 60))
        );
        for bad in [
            "",
            "1118093",
            "202302291200",
            "11180960",
            "1118093x",
            "11180930.5",
            "111809300",
            "11182400",
        ] {
            assert_eq!(stamp(bad, 2026), None, "{bad}");
        }
    }

    #[test]
    fn reads_words_and_seconds() {
        assert_eq!(date("now", TODAY), Some(When::Now(0)));
        assert_eq!(date(" Yesterday ", TODAY), Some(When::Now(-DAY)));
        assert_eq!(date("tomorrow", TODAY), Some(When::Now(DAY)));
        assert_eq!(date("@0", TODAY), Some(When::Seconds(0, 0)));
        assert_eq!(
            date("@1794733687.5", TODAY),
            Some(When::Seconds(1_794_733_687, 500_000_000))
        );
        assert_eq!(date("@-1.25", TODAY), Some(When::Seconds(-2, 750_000_000)));
        for bad in ["@", "@x", "@1.", "@--1", "next week", ""] {
            assert_eq!(date(bad, TODAY), None, "{bad}");
        }
    }

    #[test]
    fn reads_dates_times_and_zones() {
        assert_eq!(date("2026-11-15", TODAY), local((2026, 11, 15), (0, 0, 0)));
        assert_eq!(
            date("2026-11-15 09:08", TODAY),
            local((2026, 11, 15), (9, 8, 0))
        );
        assert_eq!(
            date("2026-11-15T09:08:07.25Z", TODAY),
            Some(When::Civil(
                at((2026, 11, 15), (9, 8, 7)),
                250_000_000,
                Zone::Offset(0)
            ))
        );
        assert_eq!(
            date("2026-11-15 09:08:07 +01:30", TODAY),
            Some(When::Civil(
                at((2026, 11, 15), (9, 8, 7)),
                0,
                Zone::Offset(5400)
            ))
        );
        assert_eq!(
            date("2026-11-15 09:08-0500", TODAY),
            Some(When::Civil(
                at((2026, 11, 15), (9, 8, 0)),
                0,
                Zone::Offset(-18_000)
            ))
        );
        assert_eq!(
            date("2026-11-15 UTC", TODAY),
            Some(When::Civil(
                at((2026, 11, 15), (0, 0, 0)),
                0,
                Zone::Offset(0)
            ))
        );
        assert_eq!(date("12:30", TODAY), local((2026, 11, 18), (12, 30, 0)));
        for bad in [
            "2026-13-01",
            "2026-02-29",
            "2026-11-15 24:00",
            "2026-11-15 12:00:60",
            "2026-11-15 12",
            "2026-11-15x",
            "26-11-15",
            "2026-11-15 12:00 CET",
            "2026-11-15 12:00 +1",
            "2026-11-15 12:00 +15:00",
            "12:30:1",
        ] {
            assert_eq!(date(bad, TODAY), None, "{bad}");
        }
    }

    #[test]
    fn sets_both_times_unless_told_one() {
        let which = |arguments: &[&str]| match COMMAND.parse(
            arguments
                .iter()
                .map(|a| a.to_string())
                .chain(["f".to_string()]),
        ) {
            Parsed::Run(run) => Which::of(&run),
            other => panic!("{other:?}"),
        };
        let both = Which {
            access: true,
            modified: true,
        };
        assert_eq!(which(&[]), both);
        assert_eq!(which(&["-am"]), both);
        assert!(!which(&["-a"]).modified && which(&["-a"]).access);
        assert!(which(&["-m"]).modified && !which(&["-m"]).access);
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/touch.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
