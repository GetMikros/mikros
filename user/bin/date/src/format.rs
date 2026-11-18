//! Times as date writes them: the conversions of POSIX `strftime`, with
//! the flags `-`, `_`, `0` and `^`, a width, and `%N` and `%:z` besides,
//! in the C locale (MIKSPEC-0098.R53).

/// A time, and the offset and zone it is shown in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moment {
    /// Seconds since 1970-01-01T00:00:00Z.
    pub seconds: i64,
    pub nanos: u32,
    /// Seconds east of UTC.
    pub offset: i32,
    /// The zone's abbreviation, such as `CET`.
    pub zone: String,
}

/// The parts of a moment's local time.
struct Fields {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    /// From 0 for Sunday.
    weekday: u32,
    /// From 0 for January 1.
    yearday: u32,
}

impl Fields {
    fn of(moment: &Moment) -> Fields {
        let local = moment.seconds.saturating_add(i64::from(moment.offset));
        let days = local.div_euclid(86_400);
        let of_day = local.rem_euclid(86_400) as u32;
        let (year, month, day) = mikros_tz::civil(days);
        Fields {
            year,
            month,
            day,
            hour: of_day / 3600,
            minute: of_day / 60 % 60,
            second: of_day % 60,
            weekday: (days + 4).rem_euclid(7) as u32,
            yearday: (days - mikros_tz::days(year, 1, 1)) as u32,
        }
    }

    /// The year and week of ISO 8601, whose weeks start on Monday and
    /// whose first week holds the year's first Thursday.
    fn iso_week(&self) -> (i64, i64) {
        let weekday = i64::from((self.weekday + 6) % 7 + 1);
        let week = (i64::from(self.yearday) + 1 - weekday + 10) / 7;
        if week < 1 {
            (self.year - 1, weeks_in(self.year - 1))
        } else if week > weeks_in(self.year) {
            (self.year + 1, 1)
        } else {
            (self.year, week)
        }
    }

    fn hour12(&self) -> i64 {
        match self.hour % 12 {
            0 => 12,
            h => i64::from(h),
        }
    }
}

/// The ISO 8601 weeks of `year`: 53 when it ends on a Thursday, or a
/// year before it on a Wednesday.
fn weeks_in(year: i64) -> i64 {
    let ends = |y: i64| (y + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400)).rem_euclid(7);
    match ends(year) == 4 || ends(year - 1) == 3 {
        true => 53,
        false => 52,
    }
}

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// What a conversion gives: a number with its default padding and
/// width, or text.
enum Piece {
    Number(i64, char, usize),
    Text(String),
}

/// `+hhmm`, with `colons` 1 `+hh:mm`, and with 2 `+hh:mm:ss`.
fn offset(seconds: i32, colons: usize) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let s = seconds.unsigned_abs();
    let (h, m, sec) = (s / 3600, s / 60 % 60, s % 60);
    match colons {
        0 => format!("{sign}{h:02}{m:02}"),
        1 => format!("{sign}{h:02}:{m:02}"),
        _ => format!("{sign}{h:02}:{m:02}:{sec:02}"),
    }
}

/// The piece of the conversion `c`, `None` for one that is not known.
fn convert(c: char, colons: usize, width: Option<usize>, f: &Fields, m: &Moment) -> Option<Piece> {
    use Piece::{Number, Text};
    let day = DAYS[f.weekday as usize];
    let month = MONTHS[f.month as usize - 1];
    let composite = |format: &str| Text(strftime(format, m));
    Some(match c {
        'a' => Text(day[..3].into()),
        'A' => Text(day.into()),
        'b' | 'h' => Text(month[..3].into()),
        'B' => Text(month.into()),
        'c' => composite("%a %b %e %H:%M:%S %Y"),
        'C' => Number(f.year.div_euclid(100), '0', 2),
        'd' => Number(i64::from(f.day), '0', 2),
        'D' | 'x' => composite("%m/%d/%y"),
        'e' => Number(i64::from(f.day), ' ', 2),
        'F' => composite("%Y-%m-%d"),
        'g' => Number(f.iso_week().0.rem_euclid(100), '0', 2),
        'G' => Number(f.iso_week().0, '0', 4),
        'H' => Number(i64::from(f.hour), '0', 2),
        'I' => Number(f.hour12(), '0', 2),
        'j' => Number(i64::from(f.yearday) + 1, '0', 3),
        'k' => Number(i64::from(f.hour), ' ', 2),
        'l' => Number(f.hour12(), ' ', 2),
        'm' => Number(i64::from(f.month), '0', 2),
        'M' => Number(i64::from(f.minute), '0', 2),
        'n' => Text("\n".into()),
        'N' => {
            let digits = width.unwrap_or(9).clamp(1, 9);
            Text(format!("{:09}", m.nanos)[..digits].into())
        }
        'p' => Text(if f.hour < 12 { "AM" } else { "PM" }.into()),
        'P' => Text(if f.hour < 12 { "am" } else { "pm" }.into()),
        'r' => composite("%I:%M:%S %p"),
        'R' => composite("%H:%M"),
        's' => Number(m.seconds, '0', 1),
        'S' => Number(i64::from(f.second), '0', 2),
        't' => Text("\t".into()),
        'T' | 'X' => composite("%H:%M:%S"),
        'u' => Number(i64::from((f.weekday + 6) % 7 + 1), '0', 1),
        'U' => Number(i64::from((f.yearday + 7 - f.weekday) / 7), '0', 2),
        'V' => Number(f.iso_week().1, '0', 2),
        'w' => Number(i64::from(f.weekday), '0', 1),
        'W' => Number(i64::from((f.yearday + 7 - (f.weekday + 6) % 7) / 7), '0', 2),
        'y' => Number(f.year.rem_euclid(100), '0', 2),
        'Y' => Number(f.year, '0', 4),
        'z' => Text(offset(m.offset, colons)),
        'Z' => Text(m.zone.clone()),
        '%' => Text("%".into()),
        _ => return None,
    })
}

/// `number` padded with `pad` to `width`, after its sign.
fn padded(number: i64, pad: Option<char>, width: usize) -> String {
    let digits = number.unsigned_abs().to_string();
    let sign = if number < 0 { "-" } else { "" };
    let fill = width.saturating_sub(digits.len() + sign.len());
    match pad {
        None => format!("{sign}{digits}"),
        Some('0') => format!("{sign}{}{digits}", "0".repeat(fill)),
        Some(c) => format!("{}{sign}{digits}", c.to_string().repeat(fill)),
    }
}

/// `format` with each conversion replaced for `moment`. An unknown
/// conversion is written as it is.
pub fn strftime(format: &str, moment: &Moment) -> String {
    let fields = Fields::of(moment);
    let mut out = String::new();
    let mut rest = format;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let spec = &rest[at + 1..];
        let mut chars = spec.char_indices().peekable();
        // `Some(None)` for no padding.
        let mut pad: Option<Option<char>> = None;
        let mut upper = false;
        while let Some(&(_, c)) = chars.peek() {
            match c {
                '-' => pad = Some(None),
                '_' => pad = Some(Some(' ')),
                '0' => pad = Some(Some('0')),
                '^' => upper = true,
                _ => break,
            }
            chars.next();
        }
        let mut width: Option<usize> = None;
        while let Some(&(_, c)) = chars.peek() {
            let Some(digit) = c.to_digit(10) else { break };
            width = Some(
                width
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit as usize),
            );
            chars.next();
        }
        let mut colons = 0;
        while chars.next_if(|&(_, c)| c == ':').is_some() {
            colons += 1;
        }
        // The modifiers E and O choose other forms that the C locale
        // does not have.
        chars.next_if(|&(_, c)| c == 'E' || c == 'O');
        let Some((i, c)) = chars.next() else {
            out.push('%');
            out.push_str(spec);
            return out;
        };
        let end = i + c.len_utf8();
        let piece = match (c, colons) {
            ('z', _) | (_, 0) => convert(c, colons, width, &fields, moment),
            _ => None,
        };
        let text = match piece {
            None => format!("%{}", &spec[..end]),
            Some(Piece::Number(n, default_pad, default_width)) => padded(
                n,
                pad.unwrap_or(Some(default_pad)),
                width.unwrap_or(default_width),
            ),
            Some(Piece::Text(text)) if c == 'N' => text,
            Some(Piece::Text(text)) => {
                let fill = width.unwrap_or(0).saturating_sub(text.chars().count());
                let c = match pad {
                    Some(Some('0')) => '0',
                    _ => ' ',
                };
                format!("{}{text}", c.to_string().repeat(fill))
            }
        };
        match upper {
            true => out.push_str(&text.to_uppercase()),
            false => out.push_str(&text),
        }
        rest = &spec[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-15T09:08:07.123456789Z, a Sunday, at `offset` in `zone`.
    fn moment(offset: i32, zone: &str) -> Moment {
        Moment {
            seconds: 1_794_733_687,
            nanos: 123_456_789,
            offset,
            zone: zone.into(),
        }
    }

    fn at(year: i64, month: u32, day: u32) -> Moment {
        Moment {
            seconds: mikros_tz::days(year, month, day) * 86_400,
            nanos: 0,
            offset: 0,
            zone: "UTC".into(),
        }
    }

    #[test]
    fn writes_the_usual_forms() {
        let m = moment(3600, "CET");
        assert_eq!(
            strftime("%Y-%m-%d %H:%M:%S %Z", &m),
            "2026-11-15 10:08:07 CET"
        );
        assert_eq!(strftime("%F %T%:z", &m), "2026-11-15 10:08:07+01:00");
        assert_eq!(
            strftime("%a, %d %b %Y %H:%M:%S %z", &m),
            "Sun, 15 Nov 2026 10:08:07 +0100"
        );
        assert_eq!(strftime("%c", &m), "Sun Nov 15 10:08:07 2026");
        assert_eq!(
            strftime("%D %x %X %R %r", &m),
            "11/15/26 11/15/26 10:08:07 10:08 10:08:07 AM"
        );
        assert_eq!(
            strftime("%::z", &moment(-(5 * 3600 + 30 * 60), "X")),
            "-05:30:00"
        );
    }

    #[test]
    fn writes_each_conversion() {
        let m = moment(0, "UTC");
        let cases = [
            ("%a %A %b %h %B", "Sun Sunday Nov Nov November"),
            ("%C %y %Y %G %g", "20 26 2026 2026 26"),
            ("%d %e %j", "15 15 319"),
            ("%H %I %k %l %M %S %p %P", "09 09  9  9 08 07 AM am"),
            ("%m %u %w %U %W %V", "11 7 0 46 45 46"),
            ("%s %N %3N", "1794733687 123456789 123"),
            ("a%%b%nc%td", "a%b\nc\td"),
        ];
        for (format, expected) in cases {
            assert_eq!(strftime(format, &m), expected, "{format}");
        }
    }

    #[test]
    fn takes_flags_and_widths() {
        let m = moment(0, "UTC");
        assert_eq!(
            strftime("%-d %_H %-H %05Y %^a %^B %10A|", &m),
            "15  9 9 02026 SUN NOVEMBER     Sunday|"
        );
        assert_eq!(strftime("%-m/%-e %_3j %Ey %Od", &m), "11/15 319 26 15");
    }

    #[test]
    fn writes_unknown_conversions_as_they_are() {
        let m = moment(0, "UTC");
        assert_eq!(strftime("%q %:d 100%", &m), "%q %:d 100%");
        assert_eq!(strftime("%-", &m), "%-");
    }

    #[test]
    fn counts_weeks_as_iso_8601_does() {
        // 2021-01-03, a Sunday, is in the last week of 2020, which has 53.
        assert_eq!(strftime("%G-W%V-%u", &at(2021, 1, 3)), "2020-W53-7");
        assert_eq!(strftime("%G-W%V-%u", &at(2021, 1, 4)), "2021-W01-1");
        // 2024-12-30, a Monday, is in the first week of 2025.
        assert_eq!(
            strftime("%G-W%V-%u %U %W", &at(2024, 12, 30)),
            "2025-W01-1 52 53"
        );
        assert_eq!(strftime("%j %U %W", &at(2023, 1, 1)), "001 01 00");
    }

    #[test]
    fn writes_times_before_1970_and_noon() {
        let m = Moment {
            seconds: -1,
            nanos: 0,
            offset: 0,
            zone: "UTC".into(),
        };
        assert_eq!(strftime("%F %T %a %s", &m), "1969-12-31 23:59:59 Wed -1");
        let noon = Moment {
            seconds: 12 * 3600,
            ..m
        };
        assert_eq!(strftime("%I %l %p", &noon), "12 12 PM");
    }
}
