//! The time registers of the PC's CMOS real-time clock and the date and
//! time they hold: decoding them in the format that status register B
//! gives, binary or binary-coded decimal and 12- or 24-hour, and encoding
//! a time in that format. Times are seconds since 1970-01-01T00:00:00 of
//! whatever standard the clock keeps; nothing here converts them.

#[cfg(test)]
mod tests;

/// The registers' indexes.
pub const SECONDS: u8 = 0x00;
pub const MINUTES: u8 = 0x02;
pub const HOURS: u8 = 0x04;
pub const WEEKDAY: u8 = 0x06;
pub const DAY: u8 = 0x07;
pub const MONTH: u8 = 0x08;
pub const YEAR: u8 = 0x09;
pub const STATUS_A: u8 = 0x0A;
pub const STATUS_B: u8 = 0x0B;
/// Status register A's bit that is set while the clock updates its time
/// registers.
pub const UPDATE_IN_PROGRESS: u8 = 1 << 7;
/// Status register B's bits: updates stopped while the time is set,
/// binary rather than binary-coded decimal values, and 24-hour rather
/// than 12-hour hours.
pub const SET: u8 = 1 << 7;
pub const BINARY: u8 = 1 << 2;
pub const HOURS_24: u8 = 1 << 1;
/// The bit of a 12-hour hours register that marks the afternoon.
const PM: u8 = 1 << 7;
const SECONDS_PER_DAY: i64 = 86_400;

/// The values of the time registers, in the clock's format, and of the
/// century register when there is one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registers {
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub weekday: u8,
    pub day: u8,
    pub month: u8,
    pub year: u8,
    pub century: Option<u8>,
}

/// How the clock writes its values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub binary: bool,
    pub hours_24: bool,
}

impl Format {
    /// The format that the value `status_b` of status register B gives.
    pub fn of(status_b: u8) -> Format {
        Format {
            binary: status_b & BINARY != 0,
            hours_24: status_b & HOURS_24 != 0,
        }
    }

    /// The value of the register value `value`: None for binary-coded
    /// decimal with a digit above 9.
    fn value(self, value: u8) -> Option<u8> {
        if self.binary {
            return Some(value);
        }
        let (tens, ones) = (value >> 4, value & 0xF);
        (tens <= 9 && ones <= 9).then_some(tens * 10 + ones)
    }

    /// The register value of `value`, below 100.
    fn register(self, value: u8) -> u8 {
        match self.binary {
            true => value,
            false => ((value / 10) << 4) | (value % 10),
        }
    }
}

/// The days of month `month` of `year`.
fn days_in_month(year: i64, month: u8) -> i64 {
    let (next_year, next_month) = match month {
        12 => (year + 1, 1),
        m => (year, u32::from(m) + 1),
    };
    mikros_tz::days(next_year, next_month, 1) - mikros_tz::days(year, u32::from(month), 1)
}

/// The time that `registers` hold in `format`, the years 2000 to 2099
/// without a century register: None when they hold no valid date and
/// time. The day of the week is not read.
pub fn decode(registers: &Registers, format: Format) -> Option<i64> {
    let seconds = format.value(registers.seconds)?;
    let minutes = format.value(registers.minutes)?;
    let hours = match format.hours_24 {
        true => format.value(registers.hours)?,
        false => {
            let afternoon = registers.hours & PM != 0;
            let hours = format.value(registers.hours & !PM)?;
            if !(1..=12).contains(&hours) {
                return None;
            }
            hours % 12 + if afternoon { 12 } else { 0 }
        }
    };
    let day = format.value(registers.day)?;
    let month = format.value(registers.month)?;
    let year = format.value(registers.year)?;
    let century = match registers.century {
        Some(century) => format.value(century)?,
        None => 20,
    };
    if seconds > 59 || minutes > 59 || hours > 23 || year > 99 || century > 99 {
        return None;
    }
    if !(1..=12).contains(&month) {
        return None;
    }
    let year = i64::from(century) * 100 + i64::from(year);
    if day == 0 || i64::from(day) > days_in_month(year, month) {
        return None;
    }
    let days = mikros_tz::days(year, u32::from(month), u32::from(day));
    let time = i64::from(hours) * 3600 + i64::from(minutes) * 60 + i64::from(seconds);
    Some(days * SECONDS_PER_DAY + time)
}

/// The registers that hold the time `seconds` in `format`, with a century
/// register when `century`, and the day of the week from 1 for Sunday to
/// 7: None when the clock cannot hold the time, outside the years 2000 to
/// 2099 without a century register and 0 to 9999 with one.
pub fn encode(seconds: i64, format: Format, century: bool) -> Option<Registers> {
    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let time = seconds.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = mikros_tz::civil(days);
    let years = if century { 0..=9999 } else { 2000..=2099 };
    if !years.contains(&year) {
        return None;
    }
    // 1970-01-01 was a Thursday, day 5 of the week.
    let weekday = (days + 4).rem_euclid(7) as u8 + 1;
    let hours = (time / 3600) as u8;
    let hours = match format.hours_24 {
        true => format.register(hours),
        false => {
            let afternoon = if hours >= 12 { PM } else { 0 };
            format.register(match hours % 12 {
                0 => 12,
                hours => hours,
            }) | afternoon
        }
    };
    Some(Registers {
        seconds: format.register((time % 60) as u8),
        minutes: format.register((time / 60 % 60) as u8),
        hours,
        weekday: format.register(weekday),
        day: format.register(day as u8),
        month: format.register(month as u8),
        year: format.register((year % 100) as u8),
        century: century.then(|| format.register((year / 100) as u8)),
    })
}
