//! Numbers as the conversions of C's printf write them, for printf and
//! seq: integers in decimal, octal and hexadecimal, and floating-point
//! numbers in fixed, exponent and general form, with flags, a width and
//! a precision.

/// A conversion's flags, width and precision, and its letter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Directive {
    /// `-`: aligned to the left.
    pub minus: bool,
    /// `+`: a sign also before positive numbers.
    pub plus: bool,
    /// ` `: a space before positive numbers.
    pub space: bool,
    /// `#`: the alternative form.
    pub alt: bool,
    /// `0`: padded with zeros.
    pub zero: bool,
    pub width: Option<usize>,
    pub precision: Option<usize>,
    pub conversion: u8,
}

/// `body` after `prefix`, a sign or base, padded to the width: with
/// zeros between them when `zeros` allows it and `0` was given.
pub fn pad(d: &Directive, prefix: &str, body: &str, zeros: bool) -> String {
    let length = prefix.chars().count() + body.chars().count();
    let fill = d.width.unwrap_or(0).saturating_sub(length);
    if d.minus {
        format!("{prefix}{body}{}", " ".repeat(fill))
    } else if d.zero && zeros {
        format!("{prefix}{}{body}", "0".repeat(fill))
    } else {
        format!("{}{prefix}{body}", " ".repeat(fill))
    }
}

/// `value` by `d`'s conversion: `d` or `i` signed, `o`, `u`, `x` or
/// `X` as given, which the caller has made unsigned.
pub fn integer(d: &Directive, value: i128) -> String {
    let magnitude = value.unsigned_abs();
    let mut digits = match d.conversion {
        b'o' => format!("{magnitude:o}"),
        b'x' => format!("{magnitude:x}"),
        b'X' => format!("{magnitude:X}"),
        _ => magnitude.to_string(),
    };
    if let Some(p) = d.precision {
        if p == 0 && magnitude == 0 {
            digits.clear();
        }
        if digits.len() < p {
            digits.insert_str(0, &"0".repeat(p - digits.len()));
        }
    }
    let mut prefix = "";
    if matches!(d.conversion, b'd' | b'i') {
        prefix = sign(value < 0, d);
    } else if d.alt {
        match d.conversion {
            b'o' if !digits.starts_with('0') => digits.insert(0, '0'),
            b'x' if magnitude != 0 => prefix = "0x",
            b'X' if magnitude != 0 => prefix = "0X",
            _ => {}
        }
    }
    pad(d, prefix, &digits, d.precision.is_none())
}

fn sign(negative: bool, d: &Directive) -> &'static str {
    if negative {
        "-"
    } else if d.plus {
        "+"
    } else if d.space {
        " "
    } else {
        ""
    }
}

/// `value` by `d`'s conversion: `f`, `e` or `g`, in lowercase or
/// uppercase, with a precision of 6 when none is given.
pub fn float(d: &Directive, value: f64) -> String {
    let upper = d.conversion.is_ascii_uppercase();
    let prefix = sign(value.is_sign_negative() && !value.is_nan(), d);
    let a = value.abs();
    if !a.is_finite() {
        let body = match (a.is_nan(), upper) {
            (true, false) => "nan",
            (true, true) => "NAN",
            (false, false) => "inf",
            (false, true) => "INF",
        };
        return pad(d, prefix, body, false);
    }
    let p = d.precision.unwrap_or(6);
    let body = match d.conversion.to_ascii_lowercase() {
        b'e' => exponent(a, p, d.alt, upper),
        b'g' => general(a, p, d.alt, upper),
        _ => fixed(a, p, d.alt),
    };
    pad(d, prefix, &body, true)
}

fn fixed(a: f64, p: usize, alt: bool) -> String {
    let mut text = format!("{a:.p$}");
    if alt && p == 0 {
        text.push('.');
    }
    text
}

/// The decimal exponent of `a` written with `p` digits after the point.
fn exponent_of(a: f64, p: usize) -> i32 {
    let text = format!("{a:.p$e}");
    text.split_once('e')
        .and_then(|(_, e)| e.parse().ok())
        .unwrap_or(0)
}

fn exponent(a: f64, p: usize, alt: bool, upper: bool) -> String {
    let text = format!("{a:.p$e}");
    let (mantissa, e) = text.split_once('e').unwrap_or((&text, "0"));
    let e: i32 = e.parse().unwrap_or(0);
    let mut mantissa = mantissa.to_string();
    if alt && p == 0 {
        mantissa.push('.');
    }
    let letter = if upper { 'E' } else { 'e' };
    let sign = if e < 0 { '-' } else { '+' };
    format!("{mantissa}{letter}{sign}{:02}", e.unsigned_abs())
}

fn general(a: f64, p: usize, alt: bool, upper: bool) -> String {
    let p = p.max(1);
    let x = match a == 0.0 {
        true => 0,
        false => exponent_of(a, p - 1),
    };
    let mut text = match x < p as i32 && x >= -4 {
        true => fixed(a, (p as i32 - 1 - x) as usize, alt),
        false => exponent(a, p - 1, alt, upper),
    };
    if !alt {
        let at = text.find(['e', 'E']).unwrap_or(text.len());
        let (mantissa, rest) = text.split_at(at);
        if mantissa.contains('.') {
            let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
            text = format!("{mantissa}{rest}");
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(conversion: u8) -> Directive {
        Directive {
            conversion,
            ..Directive::default()
        }
    }

    #[test]
    fn writes_integers() {
        assert_eq!(integer(&d(b'd'), -42), "-42");
        let wide = Directive {
            width: Some(6),
            zero: true,
            ..d(b'd')
        };
        assert_eq!(integer(&wide, -42), "-00042");
        let left = Directive {
            width: Some(5),
            minus: true,
            plus: true,
            ..d(b'i')
        };
        assert_eq!(integer(&left, 7), "+7   ");
        let precise = Directive {
            precision: Some(3),
            width: Some(5),
            zero: true,
            ..d(b'd')
        };
        assert_eq!(integer(&precise, 5), "  005");
        let none = Directive {
            precision: Some(0),
            ..d(b'd')
        };
        assert_eq!(integer(&none, 0), "");
        let alt = |c| Directive { alt: true, ..d(c) };
        assert_eq!(integer(&alt(b'x'), 255), "0xff");
        assert_eq!(integer(&alt(b'X'), 255), "0XFF");
        assert_eq!(integer(&alt(b'x'), 0), "0");
        assert_eq!(integer(&alt(b'o'), 8), "010");
        assert_eq!(integer(&d(b'u'), u64::MAX.into()), "18446744073709551615");
    }

    #[test]
    fn writes_floats() {
        assert_eq!(float(&d(b'f'), 1.23456), "1.234560");
        let p = |c, p| Directive {
            precision: Some(p),
            ..d(c)
        };
        assert_eq!(float(&p(b'f', 2), 2.675), "2.67");
        assert_eq!(float(&p(b'f', 0), 2.5), "2");
        assert_eq!(float(&d(b'e'), 12345.678), "1.234568e+04");
        assert_eq!(float(&d(b'E'), 0.000123), "1.230000E-04");
        assert_eq!(float(&d(b'e'), 0.0), "0.000000e+00");
        assert_eq!(float(&d(b'g'), 100000.0), "100000");
        assert_eq!(float(&d(b'g'), 1000000.0), "1e+06");
        assert_eq!(float(&d(b'g'), 0.0001), "0.0001");
        assert_eq!(float(&d(b'g'), 0.00001), "1e-05");
        assert_eq!(float(&d(b'g'), 1.5), "1.5");
        assert_eq!(float(&d(b'g'), 0.0), "0");
        assert_eq!(float(&p(b'g', 3), 1.23456), "1.23");
        let alt = Directive {
            alt: true,
            ..d(b'g')
        };
        assert_eq!(float(&alt, 1.5), "1.50000");
        let wide = Directive {
            width: Some(8),
            precision: Some(2),
            zero: true,
            ..d(b'f')
        };
        assert_eq!(float(&wide, -1.5), "-0001.50");
        assert_eq!(float(&wide, f64::INFINITY), "     inf");
        assert_eq!(float(&d(b'F'), f64::NAN), "NAN");
        assert_eq!(float(&d(b'f'), -0.0), "-0.000000");
        let plus = Directive {
            plus: true,
            ..d(b'f')
        };
        assert_eq!(float(&plus, 1.0), "+1.000000");
    }
}
