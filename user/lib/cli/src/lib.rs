//! What the command-line utilities share: reading their arguments, the
//! text of `--help`, their exit statuses and error messages, and JSON
//! strings. The tables of `spec` describe what each utility takes;
//! `Command` is the older form, which the utilities leave as they move
//! to the tables.

extern crate alloc;

pub mod colour;
pub mod size;
pub mod spec;
pub mod time;
mod usage;

pub use colour::{Paint, Role};
pub use spec::{Kind, Operand, Opt, Parsed, Run, Spec};

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// The exit status when the utility did what it was asked.
pub const SUCCESS: u8 = 0;
/// The exit status when it failed.
pub const FAILURE: u8 = 1;
/// The exit status when its arguments are invalid.
pub const INVALID: u8 = 2;

/// What a utility takes and does, for reading its arguments and for
/// `--help`.
pub struct Command {
    /// The command's name, which starts its error messages.
    pub name: &'static str,
    /// The operands after the name, as `--help` shows them.
    pub operands: &'static str,
    /// The most operands it takes.
    pub most: usize,
    /// What it does.
    pub about: &'static str,
    /// Its options besides `--help`, each with what it does.
    pub options: &'static [(&'static str, &'static str)],
    /// Its options that take a value, the next argument, each with the
    /// name of the value, as `--help` shows it, and what it does.
    pub values: &'static [(&'static str, &'static str, &'static str)],
}

/// The arguments of one run.
#[derive(Debug, PartialEq)]
pub enum Arguments {
    /// The options given, out of the command's, the options given with
    /// a value, the last value of each, and the operands.
    Run {
        options: Vec<&'static str>,
        values: Vec<(&'static str, String)>,
        operands: Vec<String>,
    },
    /// `--help` was given.
    Help,
    /// The arguments are invalid, for the reason given.
    Invalid(String),
}

impl Command {
    /// Reads `arguments`, those after the command's name. An argument
    /// that starts with `-` and is not `-` itself is an option, until
    /// `--`, after which every argument is an operand.
    pub fn parse(&self, arguments: impl IntoIterator<Item = String>) -> Arguments {
        let mut options = Vec::new();
        let mut values: Vec<(&'static str, String)> = Vec::new();
        let mut operands = Vec::new();
        let mut ended = false;
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            if ended || argument == "-" || !argument.starts_with('-') {
                operands.push(argument);
            } else if argument == "--" {
                ended = true;
            } else if argument == "--help" {
                return Arguments::Help;
            } else if let Some((option, _)) = self.options.iter().find(|(o, _)| *o == argument) {
                if !options.contains(option) {
                    options.push(*option);
                }
            } else if let Some((option, _, _)) = self.values.iter().find(|(o, _, _)| *o == argument)
            {
                let Some(value) = arguments.next() else {
                    return Arguments::Invalid(format!("option {option} needs a value"));
                };
                values.retain(|(o, _)| o != option);
                values.push((option, value));
            } else {
                return Arguments::Invalid(format!("unknown option {argument}"));
            }
        }
        if operands.len() > self.most {
            let extra = &operands[self.most];
            return Arguments::Invalid(format!("unexpected argument {extra}"));
        }
        Arguments::Run {
            options,
            values,
            operands,
        }
    }

    /// The text of `--help`: the usage, what the command does, and its
    /// options.
    pub fn help(&self) -> String {
        let mut options: Vec<(String, &str)> = self
            .options
            .iter()
            .map(|&(o, what)| (o.into(), what))
            .collect();
        for &(option, value, what) in self.values {
            options.push((format!("{option} <{value}>"), what));
        }
        options.push(("--help".into(), "show this help and exit"));
        let width = options.iter().map(|(o, _)| o.len()).max().unwrap_or(0);
        let mut text = format!("Usage: {} [options]", self.name);
        if !self.operands.is_empty() {
            text.push(' ');
            text.push_str(self.operands);
        }
        let _ = write!(text, "\n\n{}\n\nOptions:\n", self.about);
        for (option, what) in options {
            let _ = writeln!(text, "  {option:width$}  {what}");
        }
        text
    }

    /// The error message `message`, after the command's name.
    pub fn error(&self, message: &str) -> String {
        format!("{}: {message}", self.name)
    }
}

/// The value given with `option` in `values`, those of `Arguments::Run`.
pub fn value<'a>(values: &'a [(&'static str, String)], option: &str) -> Option<&'a str> {
    values
        .iter()
        .find(|(o, _)| *o == option)
        .map(|(_, v)| v.as_str())
}

/// `text` as a JSON string, in quotes, with the characters that JSON
/// does not take as they are escaped.
pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: Command = Command {
        name: "list",
        operands: "[what]",
        most: 1,
        about: "Lists what there is.",
        options: &[("--json", "write JSON")],
        values: &[("-n", "count", "list at most count")],
    };

    fn parse(arguments: &[&str]) -> Arguments {
        LIST.parse(arguments.iter().map(|a| a.to_string()))
    }

    #[test]
    fn reads_options_and_operands() {
        let run = |options: Vec<&'static str>, operands: &[&str]| Arguments::Run {
            options,
            values: vec![],
            operands: operands.iter().map(|o| o.to_string()).collect(),
        };
        assert_eq!(parse(&[]), run(vec![], &[]));
        assert_eq!(parse(&["disk", "--json"]), run(vec!["--json"], &["disk"]));
        assert_eq!(parse(&["--json", "--json"]), run(vec!["--json"], &[]));
        assert_eq!(parse(&["-"]), run(vec![], &["-"]));
        assert_eq!(parse(&["--", "--json"]), run(vec![], &["--json"]));
    }

    #[test]
    fn reads_values_and_keeps_the_last() {
        let Arguments::Run {
            values, operands, ..
        } = parse(&["-n", "3", "disk", "-n", "--json"])
        else {
            panic!("not a run");
        };
        assert_eq!(values, vec![("-n", "--json".to_string())]);
        assert_eq!(value(&values, "-n"), Some("--json"));
        assert_eq!(value(&values, "-x"), None);
        assert_eq!(operands, vec!["disk".to_string()]);
        assert_eq!(
            parse(&["-n"]),
            Arguments::Invalid("option -n needs a value".into())
        );
    }

    #[test]
    fn help_wins_and_bad_arguments_are_named() {
        assert_eq!(parse(&["--json", "--help", "--nope"]), Arguments::Help);
        let invalid = |reason: &str| Arguments::Invalid(reason.into());
        assert_eq!(parse(&["--nope"]), invalid("unknown option --nope"));
        assert_eq!(parse(&["-x"]), invalid("unknown option -x"));
        assert_eq!(parse(&["a", "b"]), invalid("unexpected argument b"));
        assert_eq!(LIST.error("no list"), "list: no list");
    }

    #[test]
    fn help_shows_usage_about_and_options() {
        assert_eq!(
            LIST.help(),
            "Usage: list [options] [what]\n\nLists what there is.\n\nOptions:\n  \
             --json      write JSON\n  -n <count>  list at most count\n  \
             --help      show this help and exit\n"
        );
    }

    #[test]
    fn escapes_json_strings() {
        assert_eq!(json_string("plain"), "\"plain\"");
        assert_eq!(json_string("a \"b\" \\ c"), "\"a \\\"b\\\" \\\\ c\"");
        assert_eq!(json_string("1\n2\t\u{1}"), "\"1\\n2\\t\\u0001\"");
        assert_eq!(json_string("äö"), "\"äö\"");
    }
}
