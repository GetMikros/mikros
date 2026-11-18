//! msh's completion mode (MIKSPEC-0084.R69): the candidates for the
//! last of the words typed after a command's name, from the command's
//! tables, so that completion offers exactly what the command takes.

use crate::spec::{Kind, Opt, Spec};
use std::path::Path;

/// One candidate: the value, and what it is.
pub type Candidate = (String, String);

/// Lists the names of system things of a kind, such as services or
/// users, that start with a prefix; files, folders and fixed choices are
/// listed here.
pub type Lister<'a> = &'a dyn Fn(Kind, &str) -> Vec<String>;

/// The candidates for the last of `words`, which may be partial.
pub fn candidates(spec: &'static Spec, words: &[String], list: Lister) -> Vec<Candidate> {
    let (done, last) = match words.split_last() {
        Some((last, done)) => (done, last.as_str()),
        None => (&[][..], ""),
    };
    let mut path: Vec<&'static str> = Vec::new();
    let mut operands = 0usize;
    let mut ended = false;
    let mut wanting: Option<Kind> = None;
    for word in done {
        let here = spec.below(&path);
        if let Some(_kind) = wanting.take() {
            continue;
        }
        if ended || word == "-" || !word.starts_with('-') {
            match here.subcommands.iter().find(|s| s.name == word) {
                Some(sub) if operands == 0 && !ended => path.push(sub.name),
                _ => {
                    operands += 1;
                    ended |= here.stops;
                }
            }
            continue;
        }
        if word == "--" {
            ended = true;
            continue;
        }
        let options = spec.options_for(&path);
        wanting = takes_next(&options, word);
    }
    let here = spec.below(&path);
    if let Some(kind) = wanting {
        return values(kind, last, list);
    }
    if !ended && last.starts_with('-') {
        let options = spec.options_for(&path);
        if let Some((name, value)) = last.split_once('=') {
            let option = options.iter().find(|o| o.named(name));
            if let Some((_, kind)) = option.and_then(|o| o.value) {
                return values(kind, value, list)
                    .into_iter()
                    .map(|(v, d)| (format!("{name}={v}"), d))
                    .collect();
            }
            return Vec::new();
        }
        return options
            .iter()
            .flat_map(|o| names(o).into_iter().map(move |n| (n, o.help.to_string())))
            .filter(|(n, _)| n.starts_with(last))
            .collect();
    }
    let mut found: Vec<Candidate> = Vec::new();
    if operands == 0 && !ended {
        found.extend(
            here.subcommands
                .iter()
                .filter(|s| s.name.starts_with(last))
                .map(|s| (s.name.to_string(), first_line(s.about))),
        );
    }
    let operand = here
        .operands
        .get(operands)
        .or_else(|| here.operands.iter().find(|o| o.rest));
    if let Some(operand) = operand {
        found.extend(values(operand.kind, last, list));
    }
    found
}

/// The kind of the value that `word`, an option, leaves for the next
/// word to give, if any.
fn takes_next(options: &[&'static Opt], word: &str) -> Option<Kind> {
    if let Some(long) = word.strip_prefix("--") {
        if long.contains('=') {
            return None;
        }
        let option = options
            .iter()
            .find(|o| !o.long.is_empty() && o.long == long)?;
        return option.value.map(|(_, k)| k);
    }
    // In a group, the first short option that takes a value takes the
    // rest of the word, or the next word when it ends the group.
    let shorts: Vec<char> = word[1..].chars().collect();
    for (i, c) in shorts.iter().enumerate() {
        let option = options.iter().find(|o| o.short == Some(*c))?;
        if let Some((_, kind)) = option.value {
            return (i + 1 == shorts.len()).then_some(kind);
        }
    }
    None
}

/// The names by which `option` may be typed.
fn names(option: &Opt) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(s) = option.short {
        names.push(format!("-{s}"));
    }
    if !option.long.is_empty() {
        names.push(format!("--{}", option.long));
    }
    names
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").to_string()
}

/// The candidates for a value of `kind` that starts with `prefix`.
fn values(kind: Kind, prefix: &str, list: Lister) -> Vec<Candidate> {
    let plain = |names: Vec<String>| -> Vec<Candidate> {
        names
            .into_iter()
            .filter(|n| n.starts_with(prefix))
            .map(|n| (n, String::new()))
            .collect()
    };
    match kind {
        Kind::Choice(words) => plain(words.iter().map(|w| w.to_string()).collect()),
        Kind::File | Kind::Path => entries(prefix, false),
        Kind::Folder => entries(prefix, true),
        Kind::Text | Kind::Int | Kind::Size => Vec::new(),
        other => plain(list(other, prefix)),
    }
}

/// The entries of the folder of `prefix` that start with its last part,
/// folders with a `/` after them, only folders when `folders`.
fn entries(prefix: &str, folders: bool) -> Vec<Candidate> {
    let (folder, start) = match prefix.rfind('/') {
        Some(at) => (&prefix[..=at], &prefix[at + 1..]),
        None => ("", prefix),
    };
    let read = match folder {
        "" => Path::new("."),
        folder => Path::new(folder),
    };
    let Ok(listing) = std::fs::read_dir(read) else {
        return Vec::new();
    };
    let mut found: Vec<Candidate> = listing
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if !name.starts_with(start) || (name.starts_with('.') && !start.starts_with('.')) {
                return None;
            }
            let is_folder = e.metadata().is_ok_and(|m| m.is_dir());
            match (is_folder, folders) {
                (true, _) => Some((format!("{folder}{name}/"), String::new())),
                (false, false) => Some((format!("{folder}{name}"), String::new())),
                (false, true) => None,
            }
        })
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Operand;

    static TOOL: Spec = Spec {
        options: &[
            Opt::long("json", "write JSON"),
            Opt::short('n', "how many").takes("count", Kind::Int),
        ],
        subcommands: &[
            Spec {
                operands: &[Operand::one("name", Kind::Service, "the service")],
                options: &[Opt::both("force", 'f', "do it anyway")],
                ..Spec::new("stop", "Stops a service.\nMore.")
            },
            Spec {
                operands: &[
                    Operand::one("name", Kind::User, "the user"),
                    Operand::one("kind", Kind::Choice(&["standard", "admin"]), "the kind"),
                ],
                ..Spec::new("kind", "Sets a kind.")
            },
        ],
        ..Spec::new("tool", "Does things.")
    };

    fn ask(words: &[&str]) -> Vec<String> {
        let list = |kind: Kind, prefix: &str| -> Vec<String> {
            let all: &[&str] = match kind {
                Kind::Service => &["logsvc", "termsvc"],
                Kind::User => &["ada", "bob"],
                _ => &[],
            };
            all.iter()
                .filter(|n| n.starts_with(prefix))
                .map(|n| n.to_string())
                .collect()
        };
        let words: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        candidates(&TOOL, &words, &list)
            .into_iter()
            .map(|(v, _)| v)
            .collect()
    }

    #[test]
    fn offers_subcommands_options_and_values() {
        assert_eq!(ask(&[""]), ["stop", "kind"]);
        assert_eq!(ask(&["s"]), ["stop"]);
        assert_eq!(ask(&["--j"]), ["--json"]);
        assert_eq!(
            ask(&["stop", "-"]),
            ["-f", "--force", "--json", "-n", "--help", "--color"]
        );
        assert_eq!(ask(&["stop", "l"]), ["logsvc"]);
        assert_eq!(ask(&["stop", "-f", ""]), ["logsvc", "termsvc"]);
        assert_eq!(ask(&["kind", "ada", ""]), ["standard", "admin"]);
        assert_eq!(ask(&["kind", "ada", "standard", ""]), Vec::<String>::new());
        assert_eq!(ask(&["--color", "a"]), ["auto", "always"]);
        assert_eq!(ask(&["--color=n"]), ["--color=never"]);
        // A value is not offered subcommands or values of operands.
        assert_eq!(ask(&["-n", ""]), Vec::<String>::new());
        assert_eq!(ask(&["-n", "5", "s"]), ["stop"]);
    }

    #[test]
    fn describes_candidates() {
        let words = vec!["".to_string()];
        let found = candidates(&TOOL, &words, &|_, _| Vec::new());
        assert_eq!(found[0], ("stop".into(), "Stops a service.".into()));
    }

    #[test]
    fn lists_entries_of_folders() {
        let dir = std::env::temp_dir().join(format!("mikros-cli-complete-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("file"), "").unwrap();
        std::fs::write(dir.join(".hidden"), "").unwrap();
        let prefix = format!("{}/", dir.display());
        let names = |found: Vec<Candidate>| -> Vec<String> {
            found
                .into_iter()
                .map(|(v, _)| v[prefix.len()..].to_string())
                .collect()
        };
        assert_eq!(names(entries(&prefix, false)), ["file", "sub/"]);
        assert_eq!(names(entries(&prefix, true)), ["sub/"]);
        assert_eq!(names(entries(&format!("{prefix}."), false)), [".hidden"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
