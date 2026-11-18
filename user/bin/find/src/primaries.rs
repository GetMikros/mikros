//! What find takes (MIKSPEC-0098.R42). Its options, `-L`, `--json`,
//! `--help` and `--color`, are in the table of `COMMAND`; its primaries,
//! such as `-name`, are words of the expression that look like options,
//! so they have a table of their own, `PRIMARIES`, from which `--help`,
//! the declaration for msh and completion come, beside those of the
//! options (MIKSPEC-0098.R31).

use mikros_cli::complete::{Candidate, Lister, candidates as of_spec};
use mikros_cli::{Kind, Operand, Opt, Spec};
use std::fmt::Write;

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "path",
        Kind::Path,
        "the folders to walk, or the current one",
    )],
    options: &[
        Opt::short('L', "follow links, before the paths"),
        Opt::long("json", "write the entries as a JSON array of objects"),
    ],
    ..Spec::new(
        "find",
        "Walks the folders named, or the current one, and writes the path of each\n\
         entry for which the expression is true, or with -print when the expression\n\
         has no -print, -print0, -delete or -exec. Links are not followed unless -L\n\
         comes before the paths. Folders are walked in the order of their names.",
    )
};

/// The letters of `-type`.
pub const TYPES: &[&str] = &["f", "d", "l", "s"];

/// A primary or operator of the expression: its name after `-`, the
/// label and kind of its value, if it takes one, and what it does.
#[derive(Debug)]
pub struct Primary {
    pub name: &'static str,
    pub value: Option<(&'static str, Kind)>,
    pub help: &'static str,
}

const fn primary(name: &'static str, help: &'static str) -> Primary {
    Primary {
        name,
        value: None,
        help,
    }
}

const fn takes(name: &'static str, label: &'static str, kind: Kind, help: &'static str) -> Primary {
    Primary {
        name,
        value: Some((label, kind)),
        help,
    }
}

pub static PRIMARIES: &[Primary] = &[
    takes(
        "name",
        "pattern",
        Kind::Text,
        "the name matches the pattern of *, ? and [...]",
    ),
    takes(
        "iname",
        "pattern",
        Kind::Text,
        "the name matches the pattern, in any case",
    ),
    takes(
        "path",
        "pattern",
        Kind::Text,
        "the path matches the pattern",
    ),
    takes(
        "type",
        "type",
        Kind::Choice(TYPES),
        "the entry is a file, folder, link or socket: f, d, l or s",
    ),
    takes(
        "size",
        "[+|-]n[K|M|G]",
        Kind::Text,
        "the size in the unit, rounded up, is more than, less than or n; bytes without one",
    ),
    takes(
        "mtime",
        "[+|-]days",
        Kind::Text,
        "it was modified more than, less than or n whole days ago",
    ),
    takes(
        "mmin",
        "[+|-]minutes",
        Kind::Text,
        "it was modified more than, less than or n whole minutes ago",
    ),
    takes(
        "newer",
        "file",
        Kind::File,
        "it was modified after the file",
    ),
    primary("empty", "the entry is an empty file or folder"),
    takes(
        "maxdepth",
        "levels",
        Kind::Int,
        "go at most this many levels below the paths",
    ),
    takes(
        "mindepth",
        "levels",
        Kind::Int,
        "test no entry fewer levels below the paths",
    ),
    primary("print", "write the path and a newline; true"),
    primary("print0", "write the path and NUL; true"),
    primary(
        "delete",
        "remove the entry, a folder after its entries; true when removed",
    ),
    takes(
        "exec",
        "command",
        Kind::Command,
        "run the command up to ; with {} as the path, true when it succeeds, or up to {} + with many paths",
    ),
    primary("not", "true when what follows is false; also !"),
    primary(
        "a",
        "true when both sides are; also when nothing is between them",
    ),
    primary("o", "true when either side is"),
];

/// The primary that `word` names, such as `-name`.
pub fn named(word: &str) -> Option<&'static Primary> {
    let name = word.strip_prefix('-')?;
    PRIMARIES.iter().find(|p| p.name == name)
}

/// Whether the long option `word` leaves its value to the next word.
fn long_takes_next(word: &str) -> bool {
    word == "--color"
}

/// The arguments split into those for `COMMAND`, the options and then,
/// after `--`, the paths, and the words of the expression. The paths
/// are the words before the first that starts with `-` or is `!` or
/// `(`; `-L` and `--` may come before them; and long options, such as
/// `--json`, may come anywhere but in a command of `-exec` or as the
/// value of a primary.
pub fn split(arguments: &[String]) -> (Vec<String>, Vec<String>) {
    let mut options = Vec::new();
    let mut paths = Vec::new();
    let mut expression = Vec::new();
    let mut i = 0;
    let mut leading = true;
    let long = |at: usize, options: &mut Vec<String>| -> usize {
        options.push(arguments[at].clone());
        match long_takes_next(&arguments[at]) && at + 1 < arguments.len() {
            true => {
                options.push(arguments[at + 1].clone());
                at + 2
            }
            false => at + 1,
        }
    };
    while i < arguments.len() {
        let word = arguments[i].as_str();
        if leading && (word == "--" || word == "-L") {
            if word == "-L" {
                options.push(word.into());
            }
            leading = word == "-L";
            i += 1;
        } else if word.starts_with("--") && word != "--" {
            i = long(i, &mut options);
        } else if word.starts_with('-') || word == "!" || word == "(" {
            break;
        } else {
            leading = false;
            paths.push(word.to_string());
            i += 1;
        }
    }
    let mut exec = false;
    while i < arguments.len() {
        let word = arguments[i].as_str();
        if exec {
            let braces = expression.last().is_some_and(|w: &String| w == "{}");
            exec = !(word == ";" || (word == "+" && braces));
        } else if word == "-exec" {
            exec = true;
        } else if word.starts_with("--") && word != "--" {
            i = long(i, &mut options);
            continue;
        } else if named(word).is_some_and(|p| p.value.is_some()) && i + 1 < arguments.len() {
            expression.push(word.to_string());
            i += 1;
        }
        expression.push(arguments[i].clone());
        i += 1;
    }
    options.push("--".into());
    options.extend(paths);
    (options, expression)
}

/// The text of `--help`: that of the options, and the primaries.
pub fn help() -> String {
    let mut text = COMMAND
        .help(&[])
        .replacen("[path...]", "[path...] [expression]", 1);
    let mut rows: Vec<(String, &str)> = PRIMARIES
        .iter()
        .map(|p| match p.value {
            Some((label, _)) => (format!("-{} <{label}>", p.name), p.help),
            None => (format!("-{}", p.name), p.help),
        })
        .collect();
    rows.push(("! <expression>".into(), "true when the expression is false"));
    rows.push(("( <expression> )".into(), "the expression, grouped"));
    let width = rows
        .iter()
        .map(|(r, _)| r.chars().count())
        .max()
        .unwrap_or(0);
    text.push_str("\nExpression:\n");
    for (row, what) in rows {
        let _ = writeln!(text, "  {row:width$}  {what}");
    }
    text
}

/// The type that msh's `extern` writes for `kind`.
#[cfg(test)]
fn declared(kind: Option<Kind>) -> String {
    match kind {
        None => "bool".into(),
        Some(Kind::Int) => "int".into(),
        Some(Kind::File) => "file".into(),
        Some(Kind::Command) => "command".into(),
        Some(Kind::Choice(words)) => format!("[{}]", words.join(" ")),
        Some(_) => "string".into(),
    }
}

/// The declaration for msh (MIKSPEC-0084.R67): that of `COMMAND`, with
/// every primary as an option with only a short form.
#[cfg(test)]
pub fn declaration() -> String {
    let items: Vec<String> = PRIMARIES
        .iter()
        .map(|p| {
            let kind = declared(p.value.map(|(_, k)| k));
            format!("-{}: {kind} \"{}\"", p.name, p.help)
        })
        .collect();
    let end = format!(",\n    {}\n) complete\n", items.join(",\n    "));
    COMMAND.declaration().replacen("\n) complete\n", &end, 1)
}

static FILE_VALUE: Spec = Spec {
    operands: &[Operand::one("file", Kind::File, "")],
    ..Spec::new("find", "")
};
static COMMAND_VALUE: Spec = Spec {
    operands: &[Operand::one("command", Kind::Command, "")],
    ..Spec::new("find", "")
};
static TYPE_VALUE: Spec = Spec {
    operands: &[Operand::one("type", Kind::Choice(TYPES), "")],
    ..Spec::new("find", "")
};

/// The candidates for the last of `words` in msh's completion mode
/// (MIKSPEC-0084.R69): the value of the primary or option before it,
/// primaries and options, or paths.
pub fn candidates(words: &[String], list: Lister) -> Vec<Candidate> {
    let (done, last) = match words.split_last() {
        Some((last, done)) => (done, last.as_str()),
        None => (&[][..], ""),
    };
    if let Some(before) = done.last() {
        if let Some((_, kind)) = named(before).and_then(|p| p.value) {
            let spec: &'static Spec = match kind {
                Kind::File => &FILE_VALUE,
                Kind::Command => &COMMAND_VALUE,
                Kind::Choice(_) => &TYPE_VALUE,
                _ => return Vec::new(),
            };
            return of_spec(spec, &[last.to_string()], list);
        }
        if long_takes_next(before) {
            return of_spec(&COMMAND, &[before.clone(), last.to_string()], list);
        }
    }
    let mut found = of_spec(&COMMAND, &[last.to_string()], list);
    if last.starts_with('-') && !last.starts_with("--") {
        found.extend(
            PRIMARIES
                .iter()
                .map(|p| (format!("-{}", p.name), p.help.to_string()))
                .filter(|(n, _)| n.starts_with(last)),
        );
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn split_of(words: &[&str]) -> (Vec<String>, Vec<String>) {
        split(&strings(words))
    }

    #[test]
    fn splits_options_and_paths_from_the_expression() {
        let (head, expression) = split_of(&["-L", "a", "b", "-name", "x", "-print"]);
        assert_eq!(head, ["-L", "--", "a", "b"]);
        assert_eq!(expression, ["-name", "x", "-print"]);
        let (head, expression) = split_of(&["--json", "-name", "--json", "--color", "never"]);
        assert_eq!(head, ["--json", "--color", "never", "--"]);
        assert_eq!(expression, ["-name", "--json"]);
        let (head, expression) = split_of(&["!", "-type", "d"]);
        assert_eq!(head, ["--"]);
        assert_eq!(expression, ["!", "-type", "d"]);
        let (head, expression) = split_of(&["a", "-exec", "x", "--json", "{}", "+", "--json"]);
        assert_eq!(head, ["--json", "--", "a"]);
        assert_eq!(expression, ["-exec", "x", "--json", "{}", "+"]);
        let (head, expression) = split_of(&["--", "-L", "a"]);
        assert_eq!(head, ["--"]);
        assert_eq!(expression, ["-L", "a"]);
        let (head, expression) = split_of(&["a", "-L"]);
        assert_eq!(head, ["--", "a"]);
        assert_eq!(expression, ["-L"]);
        let (_, expression) = split_of(&["-exec", "-name", ";", "-name"]);
        assert_eq!(expression, ["-exec", "-name", ";", "-name"]);
    }

    #[test]
    fn helps_with_the_primaries() {
        let text = help();
        assert!(text.starts_with("Usage: find [options] [path...] [expression]\n"));
        assert!(text.contains("\n  -L "));
        assert!(text.contains("\nExpression:\n  -name <pattern> "));
        assert!(text.contains("\n  -type <type> "));
        assert!(text.contains("\n  ( <expression> )"));
    }

    fn ask(words: &[&str]) -> Vec<String> {
        candidates(&strings(words), &|_, _| Vec::new())
            .into_iter()
            .map(|(v, _)| v)
            .collect()
    }

    #[test]
    fn completes_primaries_options_and_values() {
        assert_eq!(ask(&[".", "-na"]), ["-name"]);
        assert_eq!(ask(&["-pr"]), ["-print", "-print0"]);
        assert_eq!(ask(&["--j"]), ["--json"]);
        assert_eq!(ask(&["-L"]), ["-L"]);
        assert_eq!(ask(&[".", "-type", ""]), TYPES);
        assert_eq!(ask(&["--color", "n"]), ["never"]);
        assert_eq!(ask(&["-name", ""]), Vec::<String>::new());
    }

    #[test]
    fn declares_what_it_takes() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/find.msh");
        let wanted = declaration();
        if std::env::var("MIKROS_BLESS").is_ok_and(|v| v == "1") {
            std::fs::write(path, &wanted).unwrap();
        }
        let file = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            file == wanted,
            "{path} differs from the tables; MIKROS_BLESS=1 rewrites it:\n{wanted}"
        );
    }
}
