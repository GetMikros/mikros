//! What printf takes, and how it writes its format: escapes, conversions
//! that take the arguments in turn, the format again while arguments
//! remain, `%b` with the escapes of its argument and `%q` quoted for the
//! shell (MIKSPEC-0098.R45).

use crate::values::{escape, escaped, float, integer, quote};
use mikros_cli::{Kind, Operand, Spec};
use mikros_printf::{self as numbers, Directive};

pub static COMMAND: Spec = Spec {
    operands: &[
        Operand::one(
            "format",
            Kind::Text,
            "the text to write, with escapes and conversions",
        ),
        Operand::any(
            "argument",
            Kind::Text,
            "the values that the conversions take",
        ),
    ],
    options: &[],
    // Options end at the format, so that arguments such as -5 are
    // operands.
    stops: true,
    ..Spec::new(
        "printf",
        "Writes the format with each conversion replaced by the next argument,\n\
         and again while arguments remain. It reads the escapes \\\\, \\a, \\b,\n\
         \\e, \\f, \\n, \\r, \\t, \\v, \\\", \\NNN, \\xHH, \\uHHHH and \\UHHHHHHHH, and \\c\n\
         ends the output. Conversions: %d %i %o %u %x %X %c %s %f %F %e %E %g\n\
         %G, %b (the argument with escapes) and %q (quoted for the shell).",
    )
};

/// What writing the format gave.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Written {
    pub out: Vec<u8>,
    /// The arguments that were not numbers, each as a message.
    pub errors: Vec<String>,
    /// Why the format could not be written to its end.
    pub fatal: Option<String>,
}

/// Writes `format` with `arguments`: again while arguments remain and
/// the format takes some.
pub fn printf(format: &[u8], arguments: &[String]) -> Written {
    let mut printer = Printer {
        arguments,
        next: 0,
        written: Written::default(),
    };
    loop {
        let before = printer.next;
        match printer.pass(format) {
            Err(why) => {
                printer.written.fatal = Some(why);
                break;
            }
            Ok(Pass::Stopped) => break,
            Ok(Pass::Done) => {
                if printer.next >= arguments.len() || printer.next == before {
                    break;
                }
            }
        }
    }
    printer.written
}

enum Pass {
    Done,
    /// `\c` ended the output.
    Stopped,
}

struct Printer<'a> {
    arguments: &'a [String],
    next: usize,
    written: Written,
}

impl Printer<'_> {
    /// The next argument, empty when none remains.
    fn argument(&mut self) -> &str {
        self.given().unwrap_or("")
    }

    fn given(&mut self) -> Option<&str> {
        let argument = self.arguments.get(self.next).map(String::as_str);
        self.next = (self.next + 1).min(self.arguments.len());
        argument
    }

    fn pass(&mut self, format: &[u8]) -> Result<Pass, String> {
        let mut i = 0;
        while i < format.len() {
            match format[i] {
                b'\\' => {
                    let (length, stop) = escape(&format[i..], false, &mut self.written.out)?;
                    if stop {
                        return Ok(Pass::Stopped);
                    }
                    i += length;
                }
                b'%' => {
                    let (length, stop) = self.conversion(&format[i..])?;
                    if stop {
                        return Ok(Pass::Stopped);
                    }
                    i += length;
                }
                b => {
                    self.written.out.push(b);
                    i += 1;
                }
            }
        }
        Ok(Pass::Done)
    }

    /// Writes the conversion that starts `text`, and returns its length
    /// and whether `%b` met `\c`.
    fn conversion(&mut self, text: &[u8]) -> Result<(usize, bool), String> {
        if text.get(1) == Some(&b'%') {
            self.written.out.push(b'%');
            return Ok((2, false));
        }
        let mut d = Directive::default();
        let mut i = 1;
        while let Some(&c) = text.get(i) {
            match c {
                b'-' => d.minus = true,
                b'+' => d.plus = true,
                b' ' => d.space = true,
                b'#' => d.alt = true,
                b'0' => d.zero = true,
                b'\'' => {}
                _ => break,
            }
            i += 1;
        }
        if text.get(i) == Some(&b'*') {
            let width = self.signed();
            d.minus |= width < 0;
            d.width = Some(usize::try_from(width.unsigned_abs()).unwrap_or(usize::MAX));
            i += 1;
        } else {
            let (width, length) = digits(&text[i..]);
            d.width = width;
            i += length;
        }
        if text.get(i) == Some(&b'.') {
            i += 1;
            if text.get(i) == Some(&b'*') {
                let precision = self.signed();
                d.precision = usize::try_from(precision).ok();
                i += 1;
            } else {
                let (precision, length) = digits(&text[i..]);
                d.precision = Some(precision.unwrap_or(0));
                i += length;
            }
        }
        while text.get(i).is_some_and(|c| b"hlLjzt".contains(c)) {
            i += 1;
        }
        let shown = || String::from_utf8_lossy(&text[..(i + 1).min(text.len())]).into_owned();
        let Some(&conversion) = text.get(i) else {
            return Err(format!("{}: invalid conversion specification", shown()));
        };
        d.conversion = conversion;
        let out = match conversion {
            b'd' | b'i' => numbers::integer(&d, self.signed().into()).into_bytes(),
            b'o' | b'u' | b'x' | b'X' => numbers::integer(&d, self.unsigned().into()).into_bytes(),
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' => {
                let value = self.float();
                numbers::float(&d, value).into_bytes()
            }
            b'a' | b'A' => {
                return Err(format!(
                    "{}: hexadecimal floating-point conversions are not supported",
                    shown()
                ));
            }
            b'c' => {
                let first = self.argument().chars().next().map(String::from);
                text_padded(&d, first.unwrap_or_default().as_bytes())
            }
            b's' => {
                let argument = self.argument().as_bytes().to_vec();
                text_padded(&d, cut(&argument, d.precision))
            }
            b'b' => {
                let argument = self.argument().as_bytes().to_vec();
                let mut bytes = Vec::new();
                let stop = escaped(&argument, &mut bytes)?;
                let padded = text_padded(&d, cut(&bytes, d.precision));
                self.written.out.extend(padded);
                return Ok((i + 1, stop));
            }
            b'q' => {
                let quoted = quote(self.argument());
                text_padded(&d, cut(quoted.as_bytes(), d.precision))
            }
            _ => return Err(format!("{}: invalid conversion specification", shown())),
        };
        self.written.out.extend(out);
        Ok((i + 1, false))
    }

    /// The next argument as a signed number, 0 when none remains.
    fn signed(&mut self) -> i64 {
        let Some(argument) = self.given().map(String::from) else {
            return 0;
        };
        let (value, error) = integer(&argument);
        let clamped = value.clamp(i64::MIN.into(), i64::MAX.into());
        match (error, clamped != value) {
            (Some(error), _) => self.written.errors.push(error),
            (None, true) => self.written.errors.push(range(&argument)),
            (None, false) => {}
        }
        clamped as i64
    }

    /// The next argument as an unsigned number, negative ones wrapped as
    /// C's `strtoumax` does.
    fn unsigned(&mut self) -> u64 {
        let Some(argument) = self.given().map(String::from) else {
            return 0;
        };
        let (value, error) = integer(&argument);
        let limit = i128::from(u64::MAX);
        let wrapped = match value {
            v if v < -limit || v > limit => {
                self.written.errors.push(range(&argument));
                return u64::MAX;
            }
            v if v < 0 => (limit + 1 + v) as u64,
            v => v as u64,
        };
        if let Some(error) = error {
            self.written.errors.push(error);
        }
        wrapped
    }

    fn float(&mut self) -> f64 {
        let Some(argument) = self.given().map(String::from) else {
            return 0.0;
        };
        let (value, error) = float(&argument);
        if let Some(error) = error {
            self.written.errors.push(error);
        }
        value
    }
}

fn range(argument: &str) -> String {
    format!("'{argument}': value out of range")
}

/// The number that the digits starting `text` give, and their length.
fn digits(text: &[u8]) -> (Option<usize>, usize) {
    let length = text.iter().take_while(|c| c.is_ascii_digit()).count();
    let value = std::str::from_utf8(&text[..length])
        .ok()
        .and_then(|t| t.parse().ok());
    (value, length)
}

/// `bytes`, at most `precision` of them.
fn cut(bytes: &[u8], precision: Option<usize>) -> &[u8] {
    &bytes[..precision.unwrap_or(bytes.len()).min(bytes.len())]
}

/// `body` padded with spaces to the width.
fn text_padded(d: &Directive, body: &[u8]) -> Vec<u8> {
    let fill = d.width.unwrap_or(0).saturating_sub(body.len());
    let mut out = Vec::with_capacity(body.len() + fill);
    if !d.minus {
        out.resize(fill, b' ');
    }
    out.extend_from_slice(body);
    if d.minus {
        out.resize(body.len() + fill, b' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(format: &str, arguments: &[&str]) -> String {
        let arguments: Vec<String> = arguments.iter().map(|a| a.to_string()).collect();
        let written = printf(format.as_bytes(), &arguments);
        assert_eq!(written.errors, Vec::<String>::new(), "{format}");
        assert_eq!(written.fatal, None, "{format}");
        String::from_utf8(written.out).unwrap()
    }

    #[test]
    fn ends_the_options_at_the_format() {
        let given = |a: &[&str]| match COMMAND.parse(a.iter().map(|a| a.to_string())) {
            mikros_cli::Parsed::Run(run) => run.operands,
            other => panic!("{other:?}"),
        };
        assert_eq!(given(&["%d", "-5"]), ["%d", "-5"]);
        assert_eq!(given(&["--", "-%d"]), ["-%d"]);
        assert_eq!(given(&["--color", "never", "%s", "--"]), ["%s", "--"]);
        assert_eq!(given(&["-", "x"]), ["-", "x"]);
        assert!(matches!(
            COMMAND.parse(["-x".to_string(), "%s".to_string()]),
            mikros_cli::Parsed::Invalid(_)
        ));
    }

    #[test]
    fn writes_conversions_and_reuses_the_format() {
        assert_eq!(out("%s-%d\\n", &["a", "1", "b", "2"]), "a-1\nb-2\n");
        assert_eq!(out("%s %s\\n", &["a", "b", "c"]), "a b\nc \n");
        assert_eq!(out("plain\\n", &["ignored"]), "plain\n");
        assert_eq!(
            out("%5.2f|%-4d|%04x", &["3.14159", "7", "255"]),
            " 3.14|7   |00ff"
        );
        assert_eq!(
            out("%*d|%-*s|%.*s", &["4", "1", "3", "a", "2", "abc"]),
            "   1|a  |ab"
        );
        assert_eq!(out("%c%c", &["hello", "é"]), "hé");
        assert_eq!(out("%d %d %d", &["0x10", "010", "'A"]), "16 8 65");
        assert_eq!(
            out("%u %x", &["-1", "-1"]),
            "18446744073709551615 ffffffffffffffff"
        );
        assert_eq!(
            out("%e %g %G", &["1234.5", "0.00001", "1e100"]),
            "1.234500e+03 1e-05 1E+100"
        );
        assert_eq!(out("%d%%", &[]), "0%");
        assert_eq!(out("%s|%d", &[]), "|0");
        assert_eq!(out("%i", &["-12"]), "-12");
    }

    #[test]
    fn reads_escapes() {
        assert_eq!(out("a\\tb\\101\\x41\\u00e9\\\\", &[]), "a\tbAAé\\");
        assert_eq!(out("\\q\\", &[]), "\\q\\");
        assert_eq!(out("a\\cb", &[]), "a");
        assert_eq!(out("%b", &["x\\0101\\101\\ny"]), "xAA\ny");
        assert_eq!(out("%b|%s\\n", &["a\\cb", "c", "d"]), "a");
        assert_eq!(out("[%5b]", &["\\t"]), "[    \t]");
    }

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(out("%q|%q", &["a b", "c"]), "'a b'|c");
    }

    #[test]
    fn names_what_is_not_a_number() {
        let written = printf(b"%d %d %f", &["abc".into(), "12x".into(), "1.5e".into()]);
        assert_eq!(written.out, b"0 12 1.500000");
        assert_eq!(
            written.errors,
            [
                "'abc': expected a numeric value",
                "'12x': value not completely converted",
                "'1.5e': value not completely converted"
            ]
        );
        let written = printf(b"%d", &["99999999999999999999".into()]);
        assert_eq!(written.out, i64::MAX.to_string().as_bytes());
        assert_eq!(written.errors.len(), 1);
    }

    #[test]
    fn stops_at_a_bad_conversion() {
        let written = printf(b"a%yb", &[]);
        assert_eq!(written.out, b"a");
        assert_eq!(
            written.fatal.as_deref(),
            Some("%y: invalid conversion specification")
        );
        assert!(printf(b"%", &[]).fatal.is_some());
        assert!(printf(b"%a", &["1".into()]).fatal.is_some());
        assert!(printf(b"\\x", &[]).fatal.is_some());
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/printf.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
