//! What Tab completes, and the candidates for it: the word before the
//! cursor, read in its command. The first word of a command, or one
//! after `if`, `while`, `not`, `run` or `exec`, is a command name: a
//! function, a built-in command or a program of `$env.PATH`, or a file
//! when it holds a `/`. A word that starts with `$` is a variable, and
//! one that starts with `-` an option that the command declares: a
//! function's flags, a script's `param` options, or a built-in
//! command's own. The value of a script's `path` option, and any other
//! word, is a file. A file or command name that would not stay one word
//! is put on the line quoted, and a word that starts with a quote is
//! completed inside it, which is closed once the name is complete.

use crate::ast::{Param, ParamType};

/// A way to complete the word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The text that replaces the word.
    pub text: String,
    /// What the menu shows.
    pub display: String,
    pub description: Option<String>,
    /// Whether the word is done, so that a space may follow it; a
    /// folder is not.
    pub done: bool,
}

/// The candidates for the word that starts at `start`.
#[derive(Debug, PartialEq, Eq)]
pub struct Completion {
    pub start: usize,
    pub candidates: Vec<Candidate>,
}

/// What completion asks of the shell.
pub trait Sources {
    /// The entries of `folder`, and whether each is a folder.
    fn entries(&self, folder: &str) -> Option<Vec<(String, bool)>>;
    /// The programs that the folders of `$env.PATH` hold.
    fn programs(&self) -> Vec<String>;
    /// The functions, with the names of their flags.
    fn functions(&self) -> Vec<(String, Vec<String>)>;
    /// The variables and environment variables, with their values as
    /// text.
    fn variables(&self) -> Vec<(String, String)>;
    fn environment(&self) -> Vec<(String, String)>;
    /// The options that the script at `path` declares.
    fn params(&self, path: &str) -> Option<Vec<Param>>;
    /// The current folder, which suggestions from the history prefer
    /// the entries of; none when it is not known.
    fn current_folder(&self) -> Option<String> {
        None
    }
}

/// The options of built-in commands, with what they do.
const BUILTIN_OPTIONS: [(&str, &str, &str); 13] = [
    ("cd", "-", "the previous folder"),
    ("print", "-n", "no line feed at the end"),
    ("kill", "-s", "the signal by name"),
    ("kill", "-HUP", "signal 1"),
    ("kill", "-INT", "signal 2"),
    ("kill", "-QUIT", "signal 3"),
    ("kill", "-KILL", "signal 9"),
    ("kill", "-USR1", "signal 10"),
    ("kill", "-USR2", "signal 12"),
    ("kill", "-TERM", "signal 15"),
    ("kill", "-CONT", "signal 18"),
    ("kill", "-STOP", "signal 19"),
    ("kill", "-TSTP", "signal 20"),
];

/// The words after which a command name follows.
const COMMAND_KEYWORDS: [&str; 6] = ["if", "while", "not", "run", "exec", "else"];

/// The words of the command that the cursor is in, and where each
/// starts, up to the cursor; the last is the word being completed,
/// empty after a blank.
fn words(before: &str) -> Vec<(usize, String)> {
    let mut words: Vec<(usize, String)> = Vec::new();
    let mut current: Option<(usize, String)> = None;
    let mut quote = None;
    for (i, c) in before.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.get_or_insert((i, String::new())).1.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                current.get_or_insert((i, String::new()));
            }
            (None, '|' | ';' | '&' | '(' | ')' | '{' | '}' | '\n') => {
                words.clear();
                current = None;
            }
            (None, c) if c.is_whitespace() => words.extend(current.take()),
            (None, c) => current.get_or_insert((i, String::new())).1.push(c),
        }
    }
    words.push(current.unwrap_or((before.len(), String::new())));
    words
}

fn candidate(text: String, display: String, description: Option<&str>, done: bool) -> Candidate {
    Candidate {
        text,
        display,
        description: description.map(Into::into),
        done,
    }
}

/// The characters that a name must be quoted for, besides blanks.
const QUOTED: &str = "|;&><(){}[]'\"$#";

/// `name` as completion puts it on the line: quoted when it has a blank
/// or one of `QUOTED`, or when `open` is the quote that the word starts
/// with; in single quotes, or in double quotes with `"`, `$`, `{` and
/// `\\` escaped when it holds a `'` or the word started with `"`. The
/// quote is closed once the name is `done`.
fn quoted(name: &str, open: Option<char>, done: bool) -> String {
    let needs = name
        .chars()
        .any(|c| c.is_whitespace() || QUOTED.contains(c));
    if open.is_none() && !needs {
        return name.to_owned();
    }
    let quote = match open {
        _ if name.contains('\'') => '"',
        Some(q) => q,
        None => '\'',
    };
    let mut text = String::from(quote);
    for c in name.chars() {
        if quote == '"' && matches!(c, '"' | '$' | '{' | '\\') {
            text.push('\\');
        }
        text.push(c);
    }
    if done {
        text.push(quote);
    }
    text
}

/// The candidates for `line` with the cursor at byte `cursor`.
pub fn complete(line: &str, cursor: usize, sources: &dyn Sources) -> Completion {
    let words = words(&line[..cursor]);
    let (start, word) = words.last().cloned().unwrap_or_default();
    let open = line[start..cursor]
        .chars()
        .next()
        .filter(|c| matches!(c, '\'' | '"'));
    let index = words.len() - 1;
    let mut first = 0;
    while first < index && COMMAND_KEYWORDS.contains(&words[first].1.as_str()) {
        first += 1;
    }
    let names = |mut found: Vec<Candidate>| {
        for c in &mut found {
            c.text = quoted(&c.text, open, c.done);
        }
        found
    };
    let mut candidates = if word.starts_with('$') {
        variables(&word, sources)
    } else if index == first && !word.contains('/') {
        names(commands(&word, sources))
    } else if index == first {
        names(files(&word, sources))
    } else if word.starts_with('-') {
        options(&word, &words[first..index], sources)
    } else {
        match option_type(&words[first..index], sources) {
            Some(ParamType::Path | ParamType::Bool) | None => names(files(&word, sources)),
            Some(_) => Vec::new(),
        }
    };
    candidates.sort_by(|a, b| a.text.cmp(&b.text));
    candidates.dedup_by(|a, b| a.text == b.text);
    Completion { start, candidates }
}

fn variables(word: &str, sources: &dyn Sources) -> Vec<Candidate> {
    let short = |v: &str| {
        let line = v.lines().next().unwrap_or("");
        match line.chars().count() > 30 || v.contains('\n') {
            true => format!("{}…", line.chars().take(30).collect::<String>()),
            false => line.to_owned(),
        }
    };
    if let Some(prefix) = word.strip_prefix("$env.") {
        return sources
            .environment()
            .into_iter()
            .filter(|(name, _)| name.starts_with(prefix))
            .map(|(name, value)| {
                let text = format!("$env.{name}");
                candidate(text.clone(), text, Some(&short(&value)), true)
            })
            .collect();
    }
    let prefix = &word[1..];
    let mut found: Vec<Candidate> = sources
        .variables()
        .into_iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(name, value)| {
            let text = format!("${name}");
            candidate(text.clone(), text, Some(&short(&value)), true)
        })
        .collect();
    if "env".starts_with(prefix) {
        found.push(candidate("$env.".into(), "$env.".into(), None, false));
    }
    found
}

fn commands(word: &str, sources: &dyn Sources) -> Vec<Candidate> {
    let named = |name: String, what: &str| candidate(name.clone(), name, Some(what), true);
    let functions = sources.functions().into_iter().map(|(n, _)| n);
    let builtins = crate::eval::builtins::names().into_iter().map(String::from);
    let programs = sources.programs();
    functions
        .map(|n| named(n, "function"))
        .chain(builtins.map(|n| named(n, "built-in")))
        .chain(programs.into_iter().map(|n| named(n, "program")))
        .filter(|c| c.text.starts_with(word))
        .collect()
}

/// The entries of the folder that `word` names up to its last `/`
/// whose names start as the rest does; hidden ones only for a rest
/// that starts with `.`.
fn files(word: &str, sources: &dyn Sources) -> Vec<Candidate> {
    let (folder, prefix) = match word.rfind('/') {
        Some(at) => (&word[..=at], &word[at + 1..]),
        None => ("", word),
    };
    let listed = match folder {
        "" => ".",
        "/" => "/",
        f => f.trim_end_matches('/'),
    };
    let Some(entries) = sources.entries(listed) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .filter(|(name, _)| prefix.starts_with('.') || !name.starts_with('.'))
        .map(|(name, folder_entry)| {
            let slash = if folder_entry { "/" } else { "" };
            candidate(
                format!("{folder}{name}{slash}"),
                format!("{name}{slash}"),
                None,
                !folder_entry,
            )
        })
        .collect()
}

/// The script that `command` runs, if it runs one: a file it names, or
/// the first argument of `msh` that is not an option.
fn script(command: &[(usize, String)]) -> Option<&str> {
    let name = command.first()?.1.as_str();
    if name == "msh" {
        return command[1..]
            .iter()
            .map(|(_, w)| w.as_str())
            .find(|w| !w.starts_with('-'));
    }
    (name.contains('/') || name.ends_with(".msh")).then_some(name)
}

/// What a script's option shows: its type, and its default if any.
fn describe(param: &Param) -> String {
    let kind = format!("{:?}", param.kind).to_lowercase();
    match &param.default {
        Some(crate::ast::Expr::Literal(l)) => {
            let default = crate::value::scalar_text(&crate::value::Value::from(l));
            format!("{kind} = {default}")
        }
        Some(_) => format!("{kind}, optional"),
        None => kind,
    }
}

fn options(word: &str, command: &[(usize, String)], sources: &dyn Sources) -> Vec<Candidate> {
    let Some((_, name)) = command.first() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    if let Some((_, flags)) = sources.functions().into_iter().find(|(f, _)| f == name) {
        for flag in flags {
            let text = format!("--{flag}");
            found.push(candidate(text.clone(), text, Some("flag"), true));
        }
    } else if let Some(params) = script(command).and_then(|s| sources.params(s)) {
        for param in params {
            let text = format!("--{}", param.name);
            found.push(candidate(text.clone(), text, Some(&describe(&param)), true));
        }
    } else {
        for (_, option, what) in BUILTIN_OPTIONS.iter().filter(|(b, _, _)| b == name) {
            found.push(candidate(
                option.to_string(),
                option.to_string(),
                Some(what),
                true,
            ));
        }
    }
    found.retain(|c| c.text.starts_with(word));
    found
}

/// The type of the script option that the word before the one being
/// completed names, when it is one that takes a value.
fn option_type(command: &[(usize, String)], sources: &dyn Sources) -> Option<ParamType> {
    let previous = command.last()?.1.strip_prefix("--")?;
    let params = sources.params(script(command)?)?;
    params
        .into_iter()
        .find(|p| p.name == previous)
        .map(|p| p.kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    struct Fake;

    impl Sources for Fake {
        fn entries(&self, folder: &str) -> Option<Vec<(String, bool)>> {
            let entries: &[(&str, bool)] = match folder {
                "." => &[("notes.txt", false), ("src", true), (".hidden", false)],
                "src" => &[("main.rs", false), ("mod", true)],
                "/" => &[("usr", true), ("Users", true)],
                "odd" => &[
                    ("My Docs", true),
                    ("a b.txt", false),
                    ("it's.txt", false),
                    ("$x{y}", false),
                    ("plain", false),
                ],
                _ => return None,
            };
            Some(entries.iter().map(|(n, f)| (n.to_string(), *f)).collect())
        }
        fn programs(&self) -> Vec<String> {
            vec!["printf".into(), "cat".into()]
        }
        fn functions(&self) -> Vec<(String, Vec<String>)> {
            vec![("deploy".into(), vec!["dry".into(), "target".into()])]
        }
        fn variables(&self) -> Vec<(String, String)> {
            vec![
                ("status".into(), "0".into()),
                ("stuff".into(), "a\nb".into()),
            ]
        }
        fn environment(&self) -> Vec<(String, String)> {
            vec![
                ("PATH".into(), "/usr/bin".into()),
                ("PWD".into(), "/".into()),
            ]
        }
        fn params(&self, path: &str) -> Option<Vec<Param>> {
            if path != "./go.msh" && path != "go.msh" {
                return None;
            }
            let block =
                parse("param --count: int = 3\nparam --out: path\nparam --fast: bool").ok()?;
            Some(
                block
                    .statements
                    .into_iter()
                    .filter_map(|s| match s.kind {
                        crate::ast::StatementKind::Param(p) => Some(p),
                        _ => None,
                    })
                    .collect(),
            )
        }
    }

    fn texts(line: &str) -> Vec<String> {
        complete(line, line.len(), &Fake)
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    #[test]
    fn completes_command_names() {
        assert_eq!(texts("pr"), ["print", "printf"]);
        assert_eq!(texts("ls | de"), ["deploy"]);
        assert_eq!(texts("if ca"), ["cat"]);
        let c = complete("pri", 3, &Fake);
        assert_eq!(c.candidates[0].description.as_deref(), Some("built-in"));
        assert_eq!(c.candidates[1].description.as_deref(), Some("program"));
        assert_eq!(texts("src/m"), ["src/main.rs", "src/mod/"]);
    }

    #[test]
    fn completes_files_and_folders() {
        let c = complete("cat n", 5, &Fake);
        assert_eq!(c.start, 4);
        assert_eq!(c.candidates[0].text, "notes.txt");
        assert!(c.candidates[0].done);
        assert_eq!(texts("cat "), ["notes.txt", "src/"]);
        assert_eq!(texts("cat ."), [".hidden"]);
        assert_eq!(texts("cd /U"), ["/Users/"]);
        let c = complete("cat src/", 8, &Fake);
        assert_eq!(c.candidates[1].display, "mod/");
        assert!(!c.candidates[1].done);
        assert_eq!(texts("cat 'src/ma"), ["'src/main.rs'"]);
    }

    #[test]
    fn quotes_names_that_would_not_stay_one_word() {
        assert_eq!(texts("cat odd/a"), ["'odd/a b.txt'"]);
        assert_eq!(texts("cat odd/pl"), ["odd/plain"]);
        // A folder's quote stays open for the rest of the path.
        assert_eq!(texts("cd odd/M"), ["'odd/My Docs/"]);
        assert_eq!(texts("cat odd/i"), ["\"odd/it's.txt\""]);
        assert_eq!(texts("cat odd/$"), ["'odd/$x{y}'"]);
        // A word that starts with a quote is completed inside it.
        assert_eq!(texts("cat 'odd/p"), ["'odd/plain'"]);
        assert_eq!(texts("cat \"odd/$"), ["\"odd/\\$x\\{y}\""]);
        assert_eq!(texts("cat 'odd/My Docs/"), Vec::<String>::new());
        // Variables and options are not names.
        assert_eq!(texts("print $env.P"), ["$env.PATH", "$env.PWD"]);
    }

    #[test]
    fn completes_variables() {
        assert_eq!(texts("print $st"), ["$status", "$stuff"]);
        assert_eq!(texts("print $e"), ["$env."]);
        assert_eq!(texts("print $env.P"), ["$env.PATH", "$env.PWD"]);
        let c = complete("$stu", 4, &Fake);
        assert_eq!(c.candidates[0].description.as_deref(), Some("a…"));
    }

    #[test]
    fn completes_declared_options() {
        assert_eq!(texts("deploy x --"), ["--dry", "--target"]);
        assert_eq!(texts("./go.msh --c"), ["--count"]);
        let c = complete("msh go.msh --", 13, &Fake);
        let shown: Vec<_> = c.candidates.iter().map(|c| c.description.clone()).collect();
        assert_eq!(
            shown,
            [
                Some("int = 3".to_string()),
                Some("bool".to_string()),
                Some("path".to_string())
            ]
        );
        assert_eq!(texts("print -"), ["-n"]);
        assert_eq!(texts("kill -T"), ["-TERM", "-TSTP"]);
        // A path option's value is a file; an int's has no candidates.
        assert_eq!(texts("./go.msh --out n"), ["notes.txt"]);
        assert!(texts("./go.msh --count ").is_empty());
        assert_eq!(texts("./go.msh --fast n"), ["notes.txt"]);
    }
}
