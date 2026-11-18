//! Sizes as arguments take them and as `-h` shows them
//! (MIKSPEC-0098.R31).

/// The bytes of `text`, a number with an optional suffix: `K`, `M`, `G`
/// and `T` for powers of 1024, `KB`, `MB`, `GB` and `TB` for powers of
/// 1000, and `B` or none for bytes.
pub fn parse(text: &str) -> Option<u64> {
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, suffix) = text.split_at(digits);
    let number: u64 = number.parse().ok()?;
    let factor: u64 = match suffix {
        "" | "B" => 1,
        "K" => 1 << 10,
        "M" => 1 << 20,
        "G" => 1 << 30,
        "T" => 1 << 40,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "TB" => 1_000_000_000_000,
        _ => return None,
    };
    number.checked_mul(factor)
}

/// `bytes` with a suffix of powers of 1024, rounded up: whole numbers
/// below 1024, one decimal below 10 of a unit, whole units above.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["K", "M", "G", "T", "P", "E"];
    if bytes < 1024 {
        return bytes.to_string();
    }
    let mut unit = 0;
    let mut scale: u128 = 1024;
    while unit + 1 < UNITS.len() && u128::from(bytes) >= scale * 1024 {
        scale *= 1024;
        unit += 1;
    }
    let bytes = u128::from(bytes);
    // Tenths of the unit, rounded up.
    let tenths = (bytes * 10).div_ceil(scale);
    if tenths < 100 {
        return format!("{}.{}{}", tenths / 10, tenths % 10, UNITS[unit]);
    }
    let whole = bytes.div_ceil(scale);
    if whole >= 1024 && unit + 1 < UNITS.len() {
        return format!("1.0{}", UNITS[unit + 1]);
    }
    format!("{whole}{}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_suffixes() {
        assert_eq!(parse("12"), Some(12));
        assert_eq!(parse("4K"), Some(4096));
        assert_eq!(parse("2M"), Some(2 << 20));
        assert_eq!(parse("1G"), Some(1 << 30));
        assert_eq!(parse("3KB"), Some(3000));
        assert_eq!(parse("1MB"), Some(1_000_000));
        assert_eq!(parse("7B"), Some(7));
        assert_eq!(parse("K"), None);
        assert_eq!(parse("1Q"), None);
        assert_eq!(parse("-1"), None);
        assert_eq!(parse("99999999999T"), None);
    }

    #[test]
    fn shows_suffixes() {
        assert_eq!(human(0), "0");
        assert_eq!(human(1023), "1023");
        assert_eq!(human(1024), "1.0K");
        assert_eq!(human(1025), "1.1K");
        assert_eq!(human(4300), "4.2K");
        assert_eq!(human(10 * 1024), "10K");
        assert_eq!(human(10 * 1024 + 1), "11K");
        assert_eq!(human((1 << 20) - 1), "1.0M");
        assert_eq!(human(5 << 30), "5.0G");
        assert_eq!(human(u64::MAX), "16E");
    }
}
