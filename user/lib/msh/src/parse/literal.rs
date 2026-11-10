//! Numbers, sizes, durations and dates.

use super::{Parser, Result};
use crate::ast::{Date, Literal};

/// The size units, with how many bytes each is.
const SIZES: [(&str, i64); 11] = [
    ("b", 1),
    ("kb", 1_000),
    ("mb", 1_000_000),
    ("gb", 1_000_000_000),
    ("tb", 1_000_000_000_000),
    ("pb", 1_000_000_000_000_000),
    ("kib", 1 << 10),
    ("mib", 1 << 20),
    ("gib", 1 << 30),
    ("tib", 1 << 40),
    ("pib", 1 << 50),
];
/// The duration units, with how many nanoseconds each is.
const DURATIONS: [(&str, i64); 8] = [
    ("ns", 1),
    ("us", 1_000),
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("min", 60_000_000_000),
    ("h", 3_600_000_000_000),
    ("day", 86_400_000_000_000),
    ("wk", 604_800_000_000_000),
];

/// Whether `c` may follow a literal: it may not be part of a longer
/// word.
fn ends(c: Option<char>) -> bool {
    !matches!(c, Some(c) if c.is_alphanumeric() || c == '_')
}

impl Parser<'_> {
    /// Whether a number, size, duration or date starts here.
    pub(super) fn at_number(&self) -> bool {
        match self.peek() {
            Some(c) if c.is_ascii_digit() => true,
            Some('.') => self.peek_at(1).is_some_and(|c| c.is_ascii_digit()),
            _ => false,
        }
    }

    /// A number, size, duration or date.
    pub(super) fn number(&mut self) -> Result<Literal> {
        if let Some(date) = self.date()? {
            return Ok(Literal::Date(date));
        }
        let start = self.pos;
        let hex = self.peek() == Some('0') && matches!(self.peek_at(1), Some('x' | 'X'));
        if hex {
            self.pos += 2;
            let digits = self.take_while(|c| c.is_ascii_hexdigit() || c == '_');
            let digits: String = digits.chars().filter(|&c| c != '_').collect();
            if digits.is_empty() || !ends(self.peek()) {
                return Err(self.error_at(start, "a hexadecimal number needs digits"));
            }
            return i64::from_str_radix(&digits, 16)
                .map(Literal::Int)
                .map_err(|_| self.error_at(start, "the number is too large"));
        }
        let mut text = self.take_while(|c| c.is_ascii_digit() || c == '_');
        let mut float = false;
        if self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
            text.push('.');
            text.push_str(&self.take_while(|c| c.is_ascii_digit() || c == '_'));
            float = true;
        }
        if matches!(self.peek(), Some('e' | 'E'))
            && (self.peek_at(1).is_some_and(|c| c.is_ascii_digit())
                || (matches!(self.peek_at(1), Some('+' | '-'))
                    && self.peek_at(2).is_some_and(|c| c.is_ascii_digit())))
        {
            text.push('e');
            self.pos += 1;
            if let Some(sign @ ('+' | '-')) = self.peek() {
                text.push(sign);
                self.pos += 1;
            }
            text.push_str(&self.take_while(|c| c.is_ascii_digit()));
            float = true;
        }
        let text: String = text.chars().filter(|&c| c != '_').collect();
        let unit = self.take_while(|c| c.is_ascii_alphabetic());
        if !ends(self.peek()) {
            return Err(self.error_at(start, "a number is followed by letters it cannot take"));
        }
        let value: f64 = text
            .parse()
            .map_err(|_| self.error_at(start, "not a number"))?;
        let scaled = |per: i64| -> Result<i64> {
            let v = value * per as f64;
            if v.is_finite() && v.abs() < 9.2e18 {
                Ok(v.round() as i64)
            } else {
                Err(self.error_at(start, "the value is too large"))
            }
        };
        if unit.is_empty() {
            return match float {
                true => Ok(Literal::Float(value)),
                false => text
                    .parse()
                    .map(Literal::Int)
                    .map_err(|_| self.error_at(start, "the number is too large")),
            };
        }
        if let Some((_, per)) = SIZES.iter().find(|(u, _)| *u == unit) {
            return scaled(*per).map(Literal::Size);
        }
        if let Some((_, per)) = DURATIONS.iter().find(|(u, _)| *u == unit) {
            return scaled(*per).map(Literal::Duration);
        }
        Err(self.error_at(start, &format!("`{unit}` is not a size or duration unit")))
    }

    /// A date `YYYY-MM-DD`, with `THH:MM:SS`, fractions of a second and
    /// `Z` or an offset `+HH:MM`, if one starts here.
    fn date(&mut self) -> Result<Option<Date>> {
        let digits = |p: &Parser, at: usize, n: usize| {
            (0..n).all(|i| p.peek_at(at + i).is_some_and(|c| c.is_ascii_digit()))
        };
        if !(digits(self, 0, 4) && self.peek_at(4) == Some('-') && digits(self, 5, 2))
            || self.peek_at(7) != Some('-')
            || !digits(self, 8, 2)
        {
            return Ok(None);
        }
        let start = self.pos;
        let num = |p: &mut Parser, n: usize| -> i64 {
            let text: String = p.chars[p.pos..p.pos + n].iter().collect();
            p.pos += n;
            text.parse().unwrap_or(0)
        };
        let year = num(self, 4);
        self.pos += 1;
        let month = num(self, 2);
        self.pos += 1;
        let day = num(self, 2);
        let (mut hour, mut minute, mut second, mut nanos) = (0, 0, 0, 0u32);
        let mut offset = None;
        if self.peek() == Some('T') && digits(self, 1, 2) {
            self.pos += 1;
            hour = num(self, 2);
            if self.peek() != Some(':') || !digits(self, 1, 2) {
                return Err(self.error_at(start, "a time needs hours and minutes"));
            }
            self.pos += 1;
            minute = num(self, 2);
            if self.peek() == Some(':') && digits(self, 1, 2) {
                self.pos += 1;
                second = num(self, 2);
                if self.peek() == Some('.') && digits(self, 1, 1) {
                    self.pos += 1;
                    let fraction = self.take_while(|c| c.is_ascii_digit());
                    let padded = format!("{fraction:0<9}");
                    nanos = padded[..9].parse().unwrap_or(0);
                }
            }
            match self.peek() {
                Some('Z') => {
                    self.pos += 1;
                    offset = Some(0);
                }
                Some(sign @ ('+' | '-')) if digits(self, 1, 2) && self.peek_at(3) == Some(':') => {
                    self.pos += 1;
                    let h = num(self, 2);
                    self.pos += 1;
                    let m = num(self, 2);
                    let seconds = (h * 3600 + m * 60) as i32;
                    offset = Some(if sign == '-' { -seconds } else { seconds });
                }
                _ => {}
            }
        }
        if !ends(self.peek())
            || !(1..=12).contains(&month)
            || day < 1
            || day > days_in_month(year, month)
            || hour > 23
            || minute > 59
            || second > 59
        {
            return Err(self.error_at(start, "not a valid date"));
        }
        let local = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
        let seconds = local - i64::from(offset.unwrap_or(0));
        Ok(Some(Date {
            seconds,
            nanos,
            offset,
        }))
    }
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The days from 1970-01-01 to the date, from Howard Hinnant's algorithm.
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
