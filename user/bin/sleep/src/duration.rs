//! What sleep takes, and its durations: numbers with an optional
//! fraction and the suffixes `s`, `m`, `h` and `d` (MIKSPEC-0098.R54).

use mikros_cli::{Kind, Operand, Spec};
use std::time::Duration;

pub static COMMAND: Spec = Spec {
    operands: &[Operand::some(
        "duration",
        Kind::Text,
        "how long to wait: a number with s, m, h or d, seconds without",
    )],
    ..Spec::new(
        "sleep",
        "Waits for the sum of the durations, numbers with an optional fraction\n\
         and the suffix s for seconds, the default, m, h or d.",
    )
};

const NANOS: u128 = 1_000_000_000;

/// The duration that `text` gives, as nanoseconds: digits with an
/// optional fraction after `.`, at least one digit in all, and an
/// optional suffix; saturating where it is longer than nanoseconds hold.
pub fn nanos(text: &str) -> Option<u128> {
    let (number, unit) = match text.char_indices().last()? {
        (at, 's') => (&text[..at], 1),
        (at, 'm') => (&text[..at], 60),
        (at, 'h') => (&text[..at], 3600),
        (at, 'd') => (&text[..at], 86_400),
        _ => (text, 1),
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.len() + fraction.len() == 0 || !digits(whole) || !digits(fraction) {
        return None;
    }
    let whole: u128 = match whole {
        "" => 0,
        w => w.parse().unwrap_or(u128::MAX),
    };
    // Digits past nanoseconds are dropped.
    let mut part: u128 = 0;
    let mut scale = NANOS;
    for b in fraction.bytes().take(9) {
        scale /= 10;
        part += u128::from(b - b'0') * scale;
    }
    Some(
        whole
            .saturating_mul(NANOS)
            .saturating_add(part)
            .saturating_mul(unit),
    )
}

/// The sum of `operands`, saturating at the longest `Duration`, or the
/// first that is no duration.
pub fn total(operands: &[String]) -> Result<Duration, String> {
    let mut sum: u128 = 0;
    for operand in operands {
        let Some(n) = nanos(operand) else {
            return Err(format!("invalid duration {operand}"));
        };
        sum = sum.saturating_add(n);
    }
    let seconds = u64::try_from(sum / NANOS).unwrap_or(u64::MAX);
    Ok(Duration::new(seconds, (sum % NANOS) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total_of(operands: &[&str]) -> Result<Duration, String> {
        let operands: Vec<String> = operands.iter().map(|o| o.to_string()).collect();
        total(&operands)
    }

    #[test]
    fn reads_numbers_fractions_and_suffixes() {
        assert_eq!(nanos("2"), Some(2 * NANOS));
        assert_eq!(nanos("1.5"), Some(1_500_000_000));
        assert_eq!(nanos(".25s"), Some(250_000_000));
        assert_eq!(nanos("3."), Some(3 * NANOS));
        assert_eq!(nanos("2m"), Some(120 * NANOS));
        assert_eq!(nanos("0.5h"), Some(1800 * NANOS));
        assert_eq!(nanos("1d"), Some(86_400 * NANOS));
        assert_eq!(nanos("0.0000000019"), Some(1));
        for bad in ["", "s", ".", "-1", "1x", "1.2.3", "1 s", "+1", "1e3", "inf"] {
            assert_eq!(nanos(bad), None, "{bad}");
        }
    }

    #[test]
    fn adds_the_durations() {
        assert_eq!(
            total_of(&["1m", "30", ".5"]),
            Ok(Duration::new(90, 500_000_000))
        );
        assert_eq!(total_of(&["1", "x"]), Err("invalid duration x".to_string()));
        let huge = "9".repeat(60);
        assert_eq!(total_of(&[&huge]).map(|d| d.as_secs()), Ok(u64::MAX));
        let big = "9".repeat(20);
        assert_eq!(total_of(&[&big, &big]).map(|d| d.as_secs()), Ok(u64::MAX));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/sleep.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
