//! What echo takes, which of its arguments are options, and the text it
//! writes, with the escapes of `-e` (MIKSPEC-0098.R45).

use mikros_cli::{Kind, Operand, Opt, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any("text", Kind::Text, "the text to write")],
    options: &[
        Opt::short('n', "write no line feed at the end"),
        Opt::short('e', "read the escapes of the text, such as \\n and \\t"),
        Opt::short('E', "write backslashes as they are, the default"),
    ],
    ..Spec::new(
        "echo",
        "Writes the text given, separated by spaces, and a line feed.\n\
         Options are read only before the text: -n, -e and -E, also\n\
         together as in -ne, --color=<when>, and --help alone. Any other\n\
         argument starts the text.",
    )
};

/// How echo writes, from its leading options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct How {
    /// No line feed at the end, with `-n`.
    pub no_newline: bool,
    /// The escapes are read, with `-e` after any `-E`.
    pub escapes: bool,
}

/// Whether `argument` is a group of echo's short options.
fn is_short(argument: &str) -> bool {
    argument.len() > 1
        && argument.starts_with('-')
        && argument[1..].chars().all(|c| matches!(c, 'n' | 'e' | 'E'))
}

/// Whether `argument` sets the colour, which echo takes as every utility
/// does, though it writes no colour.
fn is_colour(argument: &str) -> bool {
    matches!(
        argument,
        "--color=auto" | "--color=always" | "--color=never"
    )
}

/// The arguments for the shared parser, with `--` after the leading
/// options so that the rest is text, and how echo writes. `--help` is an
/// option only as the sole argument.
pub fn arguments(arguments: Vec<String>) -> (Vec<String>, How) {
    if arguments.len() == 1 && arguments[0] == "--help" {
        return (arguments, How::default());
    }
    let mut how = How::default();
    let leading = arguments
        .iter()
        .take_while(|a| is_short(a) || is_colour(a))
        .count();
    for argument in &arguments[..leading] {
        if !is_short(argument) {
            continue;
        }
        for c in argument[1..].chars() {
            match c {
                'n' => how.no_newline = true,
                'e' => how.escapes = true,
                _ => how.escapes = false,
            }
        }
    }
    let mut parsed: Vec<String> = arguments[..leading].to_vec();
    parsed.push("--".into());
    parsed.extend_from_slice(&arguments[leading..]);
    (parsed, how)
}

/// What echo writes for `texts`.
pub fn text(texts: &[String], how: How) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, text) in texts.iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        match how.escapes {
            true => {
                if !unescape(text.as_bytes(), &mut out) {
                    return out;
                }
            }
            false => out.extend_from_slice(text.as_bytes()),
        }
    }
    if !how.no_newline {
        out.push(b'\n');
    }
    out
}

/// Appends `text` to `out` with its escapes read; false when `\c` ends
/// the output there.
fn unescape(text: &[u8], out: &mut Vec<u8>) -> bool {
    let mut i = 0;
    while i < text.len() {
        let b = text[i];
        if b != b'\\' || i + 1 == text.len() {
            out.push(b);
            i += 1;
            continue;
        }
        let next = text[i + 1];
        i += 2;
        let byte = match next {
            b'\\' => b'\\',
            b'a' => 0x07,
            b'b' => 0x08,
            b'c' => return false,
            b'e' => 0x1b,
            b'f' => 0x0c,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 0x0b,
            b'0' => {
                let (value, used) = digits(&text[i..], 3, 8);
                i += used;
                value
            }
            b'x' => {
                let (value, used) = digits(&text[i..], 2, 16);
                if used == 0 {
                    out.extend_from_slice(b"\\x");
                    continue;
                }
                i += used;
                value
            }
            other => {
                out.push(b'\\');
                other
            }
        };
        out.push(byte);
    }
    true
}

/// The value of at most `most` digits of `radix` at the start of `text`,
/// as a byte, and how many there were.
fn digits(text: &[u8], most: usize, radix: u32) -> (u8, usize) {
    let mut value: u32 = 0;
    let mut used = 0;
    while used < most {
        let Some(digit) = text.get(used).and_then(|&b| (b as char).to_digit(radix)) else {
            break;
        };
        value = value * radix + digit;
        used += 1;
    }
    ((value & 0xff) as u8, used)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::{Parsed, Run};

    fn echo(given: &[&str]) -> Vec<u8> {
        let (parsed, how) = arguments(given.iter().map(|a| a.to_string()).collect());
        let run: Run = match COMMAND.parse(parsed) {
            Parsed::Run(run) => run,
            other => panic!("{other:?}"),
        };
        text(&run.operands, how)
    }

    #[test]
    fn writes_operands_with_spaces_and_a_line_feed() {
        assert_eq!(echo(&["a", "b  c"]), b"a b  c\n");
        assert_eq!(echo(&[]), b"\n");
        assert_eq!(echo(&["-n", "a"]), b"a");
        assert_eq!(echo(&["-n"]), b"");
    }

    #[test]
    fn reads_options_only_before_the_text() {
        assert_eq!(echo(&["a", "-n"]), b"a -n\n");
        assert_eq!(echo(&["-x", "a"]), b"-x a\n");
        assert_eq!(echo(&["--", "a"]), b"-- a\n");
        assert_eq!(echo(&["-", "a"]), b"- a\n");
        assert_eq!(echo(&["-nx"]), b"-nx\n");
        assert_eq!(echo(&["--help", "a"]), b"--help a\n");
        assert_eq!(echo(&["--color=never", "-n", "a"]), b"a");
        assert_eq!(echo(&["--color=red"]), b"--color=red\n");
        let (parsed, _) = arguments(vec!["--help".into()]);
        assert_eq!(COMMAND.parse(parsed), Parsed::Help(vec![]));
    }

    #[test]
    fn reads_escapes_with_e_only() {
        assert_eq!(echo(&["a\\tb"]), b"a\\tb\n");
        assert_eq!(echo(&["-e", "a\\tb\\n"]), b"a\tb\n\n");
        assert_eq!(echo(&["-eE", "a\\tb"]), b"a\\tb\n");
        assert_eq!(echo(&["-Ee", "a\\tb"]), b"a\tb\n");
        assert_eq!(
            echo(&["-e", "\\\\\\a\\b\\e\\f\\r\\v"]),
            b"\\\x07\x08\x1b\x0c\r\x0b\n"
        );
        assert_eq!(echo(&["-e", "\\0101\\0\\x41\\x4a2\\xg"]), b"A\0AJ2\\xg\n");
        assert_eq!(echo(&["-e", "\\q", "\\"]), b"\\q \\\n");
        assert_eq!(echo(&["-e", "a\\cb", "c"]), b"a");
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/echo.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
