//! Time zones from TZif files (RFC 8536), as `zic` compiles the IANA time
//! zone database: the offset of local time from UTC at a UTC time, from
//! the file's transitions and, after the last of them, the POSIX TZ rule
//! of its footer; and the UTC time of a local time, taking the earlier of
//! two offsets when the local time occurs twice.

#![no_std]

extern crate alloc;

#[cfg(test)]
mod tests;

use alloc::vec::Vec;

/// Why a file is no TZif file that this reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalid;

/// A zone: its transitions, oldest first, each with the offset from then
/// on, the offset before the first, and the rule after the last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Zone {
    transitions: Vec<(i64, i32)>,
    initial: i32,
    rule: Option<Rule>,
}

/// A POSIX TZ rule: a standard offset, and a daylight one with the times
/// it starts and ends, as offsets of local time from UTC in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rule {
    standard: i32,
    daylight: Option<(i32, Date, i32, Date, i32)>,
}

/// The day a rule's daylight time starts or ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Date {
    /// `Jn`: day n of the year from 1, never February 29.
    Julian(u16),
    /// `n`: day n of the year from 0, February 29 counted.
    Zero(u16),
    /// `Mm.w.d`: day d of week w of month m, week 5 the last.
    Month(u8, u8, u8),
}

fn be32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be64(b: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The counts of a header at `at`: isutcnt, isstdcnt, leapcnt, timecnt,
/// typecnt and charcnt.
fn counts(b: &[u8], at: usize) -> Option<[usize; 6]> {
    if b.get(at..at + 4)? != b"TZif" {
        return None;
    }
    let mut counts = [0; 6];
    for (i, count) in counts.iter_mut().enumerate() {
        *count = usize::try_from(be32(b, at + 20 + 4 * i)?).ok()?;
    }
    Some(counts)
}

/// The bytes of the data block of a header with `counts`, with times of
/// `time` bytes.
fn data_len(counts: [usize; 6], time: usize) -> usize {
    let [isut, isstd, leap, times, types, chars] = counts;
    times * time + times + types * 6 + chars + leap * (time + 4) + isstd + isut
}

impl Zone {
    /// The zone of the TZif file `bytes`, of version 2 or later, whose
    /// 64-bit data and footer are read.
    pub fn parse(bytes: &[u8]) -> Result<Zone, Invalid> {
        Self::parse_inner(bytes).ok_or(Invalid)
    }

    fn parse_inner(b: &[u8]) -> Option<Zone> {
        let first = counts(b, 0)?;
        if b[4] < b'2' {
            return None;
        }
        let second = 44 + data_len(first, 4);
        let [_, _, _, times, types, _] = counts(b, second)?;
        let at = second + 44;
        let mut transitions = Vec::with_capacity(times);
        let index_at = at + times * 8;
        let types_at = index_at + times;
        let offset_of = |i: usize| -> Option<i32> {
            if i >= types {
                return None;
            }
            be32(b, types_at + i * 6)
        };
        for i in 0..times {
            let time = be64(b, at + i * 8)?;
            let ty = usize::from(*b.get(index_at + i)?);
            transitions.push((time, offset_of(ty)?));
        }
        // The type of times before the first transition is type 0.
        let initial = offset_of(0)?;
        let footer_at = second + 44 + data_len(counts(b, second)?, 8);
        let footer = b.get(footer_at..)?;
        let rule = match footer {
            [b'\n', rest @ ..] => {
                let end = rest.iter().position(|&c| c == b'\n')?;
                let text = core::str::from_utf8(&rest[..end]).ok()?;
                if text.is_empty() {
                    None
                } else {
                    Some(Rule::parse(text)?)
                }
            }
            _ => None,
        };
        Some(Zone {
            transitions,
            initial,
            rule,
        })
    }

    /// The offset of local time from UTC, in seconds, at the UTC time
    /// `utc`, in seconds since 1970-01-01T00:00:00Z.
    pub fn offset_at(&self, utc: i64) -> i32 {
        match self.transitions.partition_point(|&(t, _)| t <= utc) {
            0 => self.initial,
            n if n == self.transitions.len() => match &self.rule {
                Some(rule) => rule.offset_at(utc),
                None => self.transitions[n - 1].1,
            },
            n => self.transitions[n - 1].1,
        }
    }

    /// The UTC time of the local time `local`, both in seconds since
    /// 1970-01-01T00:00:00 of their own: with the earlier of two offsets
    /// when the local time occurs twice, and when it does not occur, as
    /// in a gap of a change to daylight time, with the offset before the
    /// gap.
    pub fn utc_of_local(&self, local: i64) -> i64 {
        // Offsets lie within a day of UTC; try each that applies near.
        let candidates = [
            self.offset_at(local - 86_400),
            self.offset_at(local),
            self.offset_at(local + 86_400),
        ];
        let mut best: Option<(i64, i32)> = None;
        for offset in candidates {
            let utc = local - i64::from(offset);
            if self.offset_at(utc) == offset {
                // Of two that fit, the earlier UTC time.
                if best.is_none_or(|(b, _)| utc < b) {
                    best = Some((utc, offset));
                }
            }
        }
        match best {
            Some((utc, _)) => utc,
            // In a gap, the offset before it.
            None => local - i64::from(candidates[0]),
        }
    }
}

impl Rule {
    /// The rule of the TZ string `text`, such as `CET-1CEST,M3.5.0,M10.5.0/3`.
    fn parse(text: &str) -> Option<Rule> {
        let mut s = text.as_bytes();
        name(&mut s)?;
        // POSIX offsets are west of UTC; ours are east.
        let standard = -offset(&mut s)?;
        if s.is_empty() {
            return Some(Rule {
                standard,
                daylight: None,
            });
        }
        name(&mut s)?;
        let daylight = match s.first() {
            Some(b',') => standard + 3600,
            _ => -offset(&mut s)?,
        };
        s = s.strip_prefix(b",")?;
        let (start, start_time) = date(&mut s)?;
        s = s.strip_prefix(b",")?;
        let (end, end_time) = date(&mut s)?;
        if !s.is_empty() {
            return None;
        }
        Some(Rule {
            standard,
            daylight: Some((daylight, start, start_time, end, end_time)),
        })
    }

    fn offset_at(&self, utc: i64) -> i32 {
        let Some((daylight, start, start_time, end, end_time)) = self.daylight else {
            return self.standard;
        };
        let year = civil(utc.div_euclid(86_400)).0;
        // Daylight time starts at a standard local time and ends at a
        // daylight one.
        let begins = start.day(year) * 86_400 + i64::from(start_time) - i64::from(self.standard);
        let ends = end.day(year) * 86_400 + i64::from(end_time) - i64::from(daylight);
        let in_daylight = if begins < ends {
            begins <= utc && utc < ends
        } else {
            // Southern hemisphere: daylight time spans the new year.
            utc < ends || begins <= utc
        };
        if in_daylight { daylight } else { self.standard }
    }
}

/// Skips a zone abbreviation: letters, or anything within `<` and `>`.
fn name(s: &mut &[u8]) -> Option<()> {
    if let Some(rest) = s.strip_prefix(b"<") {
        let end = rest.iter().position(|&c| c == b'>')?;
        *s = &rest[end + 1..];
        return Some(());
    }
    let len = s.iter().take_while(|c| c.is_ascii_alphabetic()).count();
    if len < 3 {
        return None;
    }
    *s = &s[len..];
    Some(())
}

/// A decimal number.
fn number(s: &mut &[u8]) -> Option<i64> {
    let len = s.iter().take_while(|c| c.is_ascii_digit()).count();
    if len == 0 {
        return None;
    }
    let n = core::str::from_utf8(&s[..len]).ok()?.parse().ok()?;
    *s = &s[len..];
    Some(n)
}

/// A time `[+|-]hh[:mm[:ss]]`, in seconds.
fn offset(s: &mut &[u8]) -> Option<i32> {
    let sign = match s.first() {
        Some(b'-') => {
            *s = &s[1..];
            -1
        }
        Some(b'+') => {
            *s = &s[1..];
            1
        }
        _ => 1,
    };
    let mut seconds = number(s)? * 3600;
    for unit in [60, 1] {
        match s.strip_prefix(b":") {
            Some(rest) => {
                *s = rest;
                seconds += number(s)? * unit;
            }
            None => break,
        }
    }
    i32::try_from(sign * seconds).ok()
}

/// A rule date with its optional time, 02:00 when left out.
fn date(s: &mut &[u8]) -> Option<(Date, i32)> {
    let date = match s.first()? {
        b'J' => {
            *s = &s[1..];
            Date::Julian(u16::try_from(number(s)?).ok()?)
        }
        b'M' => {
            *s = &s[1..];
            let m = number(s)?;
            *s = s.strip_prefix(b".")?;
            let w = number(s)?;
            *s = s.strip_prefix(b".")?;
            let d = number(s)?;
            if !(1..=12).contains(&m) || !(1..=5).contains(&w) || d > 6 {
                return None;
            }
            Date::Month(m as u8, w as u8, d as u8)
        }
        _ => Date::Zero(u16::try_from(number(s)?).ok()?),
    };
    let time = match s.strip_prefix(b"/") {
        Some(rest) => {
            *s = rest;
            offset(s)?
        }
        None => 7200,
    };
    Some((date, time))
}

impl Date {
    /// The day of `year` that the date names, in days since 1970-01-01.
    fn day(self, year: i64) -> i64 {
        let jan1 = days(year, 1, 1);
        match self {
            Date::Julian(n) => {
                let n = i64::from(n) - 1;
                // Days after February 28 skip February 29.
                let leap = is_leap(year) && n >= 59;
                jan1 + n + i64::from(leap)
            }
            Date::Zero(n) => jan1 + i64::from(n),
            Date::Month(m, w, d) => {
                let first = days(year, u32::from(m), 1);
                // 1970-01-01 was a Thursday, day 4 of the week.
                let weekday = (first + 4).rem_euclid(7);
                let mut day =
                    first + (i64::from(d) - weekday).rem_euclid(7) + 7 * (i64::from(w) - 1);
                let next = if m == 12 {
                    days(year + 1, 1, 1)
                } else {
                    days(year, u32::from(m) + 1, 1)
                };
                // Week 5 is the last of the month.
                while day >= next {
                    day -= 7;
                }
                day
            }
        }
    }
}

/// Whether `year` of the proleptic Gregorian calendar is a leap year.
pub fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Days since 1970-01-01 of the date `year`-`month`-`day` of the
/// proleptic Gregorian calendar.
pub fn days(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The year, month and day of `days` since 1970-01-01, in the proleptic
/// Gregorian calendar.
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}
