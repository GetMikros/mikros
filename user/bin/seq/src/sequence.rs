//! What seq takes, and the numbers it prints: integers exactly when every
//! operand is one, otherwise floating-point numbers with as many decimals
//! as the first and the step have, or by a format of its own, padded with
//! zeros on request (MIKSPEC-0098.R50).

use mikros_cli::{Kind, Operand, Opt, Spec};
use mikros_printf::{self as numbers, Directive};

pub static COMMAND: Spec = Spec {
    operands: &[
        Operand::optional("first", Kind::Text, "the first number, 1 when absent"),
        Operand::optional("step", Kind::Text, "the step, 1 when absent"),
        Operand::one("last", Kind::Text, "the last number"),
    ],
    options: &[
        Opt::both(
            "separator",
            's',
            "write this between the numbers, not a line feed",
        )
        .takes("text", Kind::Text),
        Opt::both(
            "equal-width",
            'w',
            "pad the numbers with zeros to one width",
        ),
        Opt::both(
            "format",
            'f',
            "write each number by this printf format, such as %.2f",
        )
        .takes("format", Kind::Text),
    ],
    dash_operand: Some(negative),
    ..Spec::new(
        "seq",
        "Prints the numbers from first to last, a step apart, one per line.",
    )
};

/// Whether `argument` is a negative number, which is an operand though
/// it starts with `-`.
fn negative(argument: &str) -> bool {
    let Some(rest) = argument.strip_prefix('-') else {
        return false;
    };
    let lower = rest.to_ascii_lowercase();
    let bytes = rest.as_bytes();
    match bytes.first() {
        Some(c) if c.is_ascii_digit() => true,
        Some(b'.') => bytes.get(1).is_some_and(u8::is_ascii_digit),
        _ => lower.starts_with("inf"),
    }
}

/// An operand.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Number {
    pub value: f64,
    /// Its value when it is written as an integer.
    pub integer: Option<i128>,
    /// The digits after its decimal point.
    pub decimals: usize,
    /// Whether it has an exponent.
    pub exponent: bool,
}

/// The number that `text` writes, `None` when it is not one.
pub fn number(text: &str) -> Option<Number> {
    let text = text.trim_start();
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let value: f64 = text.strip_prefix('+').unwrap_or(text).parse().ok()?;
    if value.is_nan() {
        return None;
    }
    let integer = match unsigned.bytes().all(|b| b.is_ascii_digit()) {
        true => text.strip_prefix('+').unwrap_or(text).parse().ok(),
        false => None,
    };
    let exponent = unsigned.contains(['e', 'E']) && !value.is_infinite();
    let mantissa = unsigned.split(['e', 'E']).next().unwrap_or("");
    let decimals = mantissa.split_once('.').map_or(0, |(_, d)| d.len());
    Some(Number {
        value,
        integer,
        decimals,
        exponent,
    })
}

/// How each number is written.
#[derive(Clone, Debug, PartialEq)]
pub enum Format {
    /// By a format: the text before the conversion, the conversion and
    /// the text after it.
    Given(String, Directive, String),
    /// With this many decimals, padded with zeros to the width.
    Fixed(usize, usize),
    /// As `%g` does, for operands with exponents.
    General,
}

impl Format {
    pub fn write(&self, value: f64) -> String {
        match self {
            Format::Given(before, d, after) => {
                format!("{before}{}{after}", numbers::float(d, value))
            }
            Format::Fixed(decimals, width) => {
                let d = Directive {
                    zero: true,
                    width: Some(*width),
                    precision: Some(*decimals),
                    conversion: b'f',
                    ..Directive::default()
                };
                numbers::float(&d, value)
            }
            Format::General => numbers::float(
                &Directive {
                    conversion: b'g',
                    ..Directive::default()
                },
                value,
            ),
        }
    }
}

/// The format that `-f`'s `text` gives: one conversion of a
/// floating-point number, among text in which `%%` writes `%`.
pub fn given(text: &str) -> Result<Format, String> {
    let mut parts: Vec<String> = vec![String::new()];
    let mut directive = None;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c != '%' {
            if let Some(part) = parts.last_mut() {
                part.push(c);
            }
            continue;
        }
        if chars.next_if(|(_, c)| *c == '%').is_some() {
            if let Some(part) = parts.last_mut() {
                part.push('%');
            }
            continue;
        }
        if directive.is_some() {
            return Err(format!("format '{text}' has too many % directives"));
        }
        let mut d = Directive::default();
        while let Some((_, c)) = chars.next_if(|(_, c)| "-+ #0'".contains(*c)) {
            match c {
                '-' => d.minus = true,
                '+' => d.plus = true,
                ' ' => d.space = true,
                '#' => d.alt = true,
                '0' => d.zero = true,
                _ => {}
            }
        }
        let digits = |chars: &mut std::iter::Peekable<std::str::CharIndices>| {
            let mut value: Option<usize> = None;
            while let Some((_, c)) = chars.next_if(|(_, c)| c.is_ascii_digit()) {
                let digit = c.to_digit(10).unwrap_or(0) as usize;
                value = Some(value.unwrap_or(0).saturating_mul(10).saturating_add(digit));
            }
            value
        };
        d.width = digits(&mut chars);
        if chars.next_if(|(_, c)| *c == '.').is_some() {
            d.precision = Some(digits(&mut chars).unwrap_or(0));
        }
        while chars.next_if(|(_, c)| *c == 'L').is_some() {}
        match chars.next() {
            Some((_, c)) if "eEfFgG".contains(c) => d.conversion = c as u8,
            Some((_, c)) if "aA".contains(c) => {
                return Err(format!(
                    "format '{text}': hexadecimal floating-point conversions are not supported"
                ));
            }
            Some((_, c)) => return Err(format!("format '{text}' has unknown %{c} directive")),
            None => {
                let rest = &text[at..];
                return Err(format!("format '{text}' ends in %: {rest}"));
            }
        }
        directive = Some(d);
        parts.push(String::new());
    }
    match directive {
        None => Err(format!("format '{text}' has no % directive")),
        Some(d) => Ok(Format::Given(parts[0].clone(), d, parts[1].clone())),
    }
}

/// The numbers of a sequence, each as written.
#[derive(Debug)]
pub enum Numbers {
    /// From the first, a step apart, up to the last, as integers padded
    /// to the width.
    Integers {
        next: Option<i128>,
        step: i128,
        last: i128,
        width: usize,
    },
    Floats {
        first: f64,
        step: f64,
        last: f64,
        format: Format,
        index: u64,
        done: bool,
    },
}

/// The sequence that the operands, a format and `-w` ask for, or why
/// they cannot make one.
pub fn numbers(operands: &[String], format: Option<&str>, equal: bool) -> Result<Numbers, String> {
    let parsed: Vec<Number> = operands
        .iter()
        .map(|o| number(o).ok_or_else(|| format!("invalid floating point argument: '{o}'")))
        .collect::<Result<_, _>>()?;
    let one = Number {
        value: 1.0,
        integer: Some(1),
        decimals: 0,
        exponent: false,
    };
    let (first, step, last) = match parsed[..] {
        [last] => (one, one, last),
        [first, last] => (first, one, last),
        [first, step, last] => (first, step, last),
        _ => return Err("expected one to three operands".into()),
    };
    if step.value == 0.0 {
        let shown = operands.get(1).map_or("0", String::as_str);
        return Err(format!("invalid Zero increment value: '{shown}'"));
    }
    if format.is_some() && equal {
        return Err("-f and -w do not go together".into());
    }
    if let (None, Some(f), Some(s), Some(l)) = (format, first.integer, step.integer, last.integer) {
        let width = match equal {
            true => f.to_string().len().max(l.to_string().len()),
            false => 0,
        };
        return Ok(Numbers::Integers {
            next: Some(f),
            step: s,
            last: l,
            width,
        });
    }
    let format = match format {
        Some(text) => given(text)?,
        None if first.exponent || step.exponent || last.exponent => Format::General,
        None => {
            let decimals = first.decimals.max(step.decimals);
            let width = match equal {
                true => {
                    let shown = |v: f64| Format::Fixed(decimals, 0).write(v).len();
                    shown(first.value).max(shown(last.value))
                }
                false => 0,
            };
            Format::Fixed(decimals, width)
        }
    };
    Ok(Numbers::Floats {
        first: first.value,
        step: step.value,
        last: last.value,
        format,
        index: 0,
        done: false,
    })
}

impl Iterator for Numbers {
    type Item = String;

    fn next(&mut self) -> Option<String> {
        match self {
            Numbers::Integers {
                next,
                step,
                last,
                width,
            } => {
                let value = (*next)?;
                if (*step > 0 && value > *last) || (*step < 0 && value < *last) {
                    return None;
                }
                *next = value.checked_add(*step);
                let d = Directive {
                    zero: true,
                    width: Some(*width),
                    conversion: b'd',
                    ..Directive::default()
                };
                Some(numbers::integer(&d, value))
            }
            Numbers::Floats {
                first,
                step,
                last,
                format,
                index,
                done,
            } => {
                if *done {
                    return None;
                }
                let value = *first + *index as f64 * *step;
                *index += 1;
                let beyond = (*step > 0.0 && value > *last) || (*step < 0.0 && value < *last);
                if !beyond {
                    return Some(format.write(value));
                }
                // A number past the last only by rounding is written when
                // it is written as the last is.
                *done = true;
                let (shown, end) = (format.write(value), format.write(*last));
                (shown == end).then_some(shown)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(operands: &[&str], format: Option<&str>, equal: bool) -> Vec<String> {
        let operands: Vec<String> = operands.iter().map(|o| o.to_string()).collect();
        numbers(&operands, format, equal)
            .unwrap()
            .take(100)
            .collect()
    }

    #[test]
    fn counts_integers() {
        assert_eq!(seq(&["3"], None, false), ["1", "2", "3"]);
        assert_eq!(seq(&["-2", "1"], None, false), ["-2", "-1", "0", "1"]);
        assert_eq!(seq(&["10", "-3", "1"], None, false), ["10", "7", "4", "1"]);
        assert_eq!(seq(&["5", "1"], None, false), Vec::<String>::new());
        assert_eq!(seq(&["8", "10"], None, true), ["08", "09", "10"]);
        assert_eq!(seq(&["-3", "2", "1"], None, true), ["-3", "-1", "01"]);
        let big = "170141183460469231731687303715884105727";
        assert_eq!(seq(&[big, big], None, false), [big]);
    }

    #[test]
    fn counts_with_decimals() {
        assert_eq!(seq(&["1", "0.5", "2"], None, false), ["1.0", "1.5", "2.0"]);
        assert_eq!(
            seq(&["0.1", "0.1", "0.3"], None, false),
            ["0.1", "0.2", "0.3"]
        );
        assert_eq!(seq(&["0.5", "2"], None, true), ["0.5", "1.5"]);
        assert_eq!(seq(&["1", "0.5", "10"], None, true)[0], "01.0");
        assert_eq!(seq(&["1e3", "1e3"], None, false), ["1000"]);
        assert_eq!(
            seq(&["1", "3"], Some("x%04.1fy"), false),
            ["x01.0y", "x02.0y", "x03.0y"]
        );
        assert_eq!(seq(&["2"], Some("%g%%"), false), ["1%", "2%"]);
        let endless: Vec<String> = numbers(&["inf".into()], None, false)
            .unwrap()
            .take(3)
            .collect();
        assert_eq!(endless, ["1", "2", "3"]);
    }

    #[test]
    fn refuses_what_makes_no_sequence() {
        let refused = |operands: &[&str], format: Option<&str>, equal: bool| {
            let operands: Vec<String> = operands.iter().map(|o| o.to_string()).collect();
            numbers(&operands, format, equal).unwrap_err()
        };
        assert_eq!(
            refused(&["x"], None, false),
            "invalid floating point argument: 'x'"
        );
        assert_eq!(
            refused(&["nan"], None, false),
            "invalid floating point argument: 'nan'"
        );
        assert_eq!(
            refused(&["1", "0", "3"], None, false),
            "invalid Zero increment value: '0'"
        );
        assert!(refused(&["3"], Some("%f"), true).contains("-w"));
        assert!(refused(&["3"], Some("%d"), false).contains("unknown %d"));
        assert!(refused(&["3"], Some("x"), false).contains("no % directive"));
        assert!(refused(&["3"], Some("%f%f"), false).contains("too many"));
        assert!(refused(&["3"], Some("%a"), false).contains("not supported"));
    }

    #[test]
    fn takes_negative_numbers_as_operands() {
        let given = |a: &[&str]| match COMMAND.parse(a.iter().map(|a| a.to_string())) {
            mikros_cli::Parsed::Run(run) => run,
            other => panic!("{other:?}"),
        };
        assert_eq!(given(&["-5", "5"]).operands, ["-5", "5"]);
        let run = given(&["-w", "-.5", "1"]);
        assert!(run.has("-w"));
        assert_eq!(run.operands, ["-.5", "1"]);
        let run = given(&["-s", "-1", "3"]);
        assert_eq!(
            (run.value("-s"), &run.operands[..]),
            (Some("-1"), &["3".to_string()][..])
        );
        assert_eq!(given(&["-f", "%g", "-inf", "1"]).operands, ["-inf", "1"]);
        assert!(matches!(
            COMMAND.parse(["-x".to_string(), "3".to_string()]),
            mikros_cli::Parsed::Invalid(_)
        ));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/seq.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
