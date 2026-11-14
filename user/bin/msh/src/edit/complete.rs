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

use crate::ast::{Extern, Param, ParamType};
use std::rc::Rc;

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
    /// What the command `name`, or its subcommand `sub`, takes, as it
    /// was declared.
    fn declaration(&self, _name: &str, _sub: Option<&str>) -> Option<Rc<Extern>> {
        None
    }
    /// The subcommands declared for the command `name`.
    fn subcommands(&self, _name: &str) -> Vec<String> {
        Vec::new()
    }
}

/// The options of built-in commands that their declarations cannot
/// give, with what they do.
const BUILTIN_OPTIONS: [(&str, &str, &str); 11] = [
    ("cd", "-", "the previous folder"),
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

/// `word` as the shell expands it before it looks up a path: `~` or
/// `~/` at its start as `$env.HOME`, and each `$name` and `$env.NAME` as
/// its value; `None` when one is not set or reads a field, which only
/// running could tell, so that completion and colours never run
/// anything.
pub fn expanded(word: &str, sources: &dyn Sources) -> Option<String> {
    if !word.contains('$') && !word.starts_with('~') {
        return Some(word.to_owned());
    }
    let environment = sources.environment();
    let value_of = |name: &str| match name.strip_prefix("env.") {
        Some(var) => environment
            .iter()
            .find(|(n, _)| n == var)
            .map(|(_, v)| v.clone()),
        None => sources
            .variables()
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v),
    };
    let name_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    // A value that ends in `/` before a `/`, such as a home folder `/`,
    // gives one.
    let joined = |value: String, next: &str| match next.starts_with('/') {
        true => value.trim_end_matches('/').to_owned(),
        false => value,
    };
    let mut out = String::new();
    let mut rest = word;
    if let Some(after) = word.strip_prefix('~')
        && (after.is_empty() || after.starts_with('/'))
    {
        out.push_str(&joined(value_of("env.HOME")?, after));
        rest = after;
    }
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let length = after.find(|c| !name_char(c)).unwrap_or(after.len());
        let mut name = after[..length].to_owned();
        let mut used = length;
        if name == "env" && after[used..].starts_with('.') {
            let var = &after[used + 1..];
            let length = var.find(|c| !name_char(c)).unwrap_or(var.len());
            name = format!("env.{}", &var[..length]);
            used += 1 + length;
        }
        if name.is_empty() {
            out.push('$');
            rest = after;
            continue;
        }
        if after[used..].starts_with('.') && after[used + 1..].starts_with(name_char) {
            return None;
        }
        out.push_str(&joined(value_of(&name)?, &after[used..]));
        rest = &after[used..];
    }
    out.push_str(rest);
    Some(out)
}

/// `found` with the text of each quoted as completion puts it on the
/// line, `open` being the quote that the word starts with.
fn quote_names(mut found: Vec<Candidate>, open: Option<char>) -> Vec<Candidate> {
    for c in &mut found {
        c.text = quoted(&c.text, open, c.done);
    }
    found
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
    let mut candidates = if word.starts_with('$') && !word.contains('/') {
        // A whole variable that names a folder goes on with its `/`.
        let found = variables(&word, sources);
        let folder = files(&word, sources, open);
        match !folder.is_empty() && found.iter().all(|c| c.text == word) {
            true => folder,
            false => found,
        }
    } else if index == first && !word.contains('/') {
        quote_names(commands(&word, sources), open)
    } else if index == first {
        files(&word, sources, open)
    } else if word.starts_with('-') {
        let command = &words[first..index];
        let mut found = options(&word, command, sources);
        if let Some((declared, _)) = declared(command, sources) {
            found.extend(declared_options(&word, &declared));
        }
        found
    } else {
        let command = &words[first..index];
        let subcommands = match index == first + 1 {
            true => sources.subcommands(&command[0].1),
            false => Vec::new(),
        };
        let kind = match declared(command, sources) {
            Some((declared, skip)) => value_type(&declared, &command[skip..]),
            None => option_type(command, sources),
        };
        let mut found: Vec<Candidate> = subcommands
            .into_iter()
            .filter(|s| s.starts_with(&word))
            .map(|s| candidate(s.clone(), s, Some("subcommand"), true))
            .collect();
        match (kind, found.is_empty()) {
            (Some(kind), _) => found.extend(by_type(&kind, &word, sources, open)),
            // Subcommands are offered without files.
            (None, true) => found = files(&word, sources, open),
            (None, false) => {}
        }
        found
    };
    candidates.sort_by(|a, b| a.text.cmp(&b.text));
    candidates.dedup_by(|a, b| a.text == b.text);
    Completion { start, candidates }
}

/// The declaration of the command whose words are `command`, its
/// subcommand's when the second word is one, with how many words name
/// it.
fn declared(command: &[(usize, String)], sources: &dyn Sources) -> Option<(Rc<Extern>, usize)> {
    let name = &command.first()?.1;
    if let Some((_, sub)) = command.get(1)
        && let Some(declared) = sources.declaration(name, Some(sub))
    {
        return Some((declared, 2));
    }
    sources.declaration(name, None).map(|d| (d, 1))
}

/// The options of `declared` that start as `word` does, long and short,
/// with their descriptions.
fn declared_options(word: &str, declared: &Extern) -> Vec<Candidate> {
    let mut found = Vec::new();
    for option in &declared.options {
        let forms = std::iter::once(format!("--{}", option.name))
            .chain(option.short.iter().map(|s| format!("-{s}")));
        for text in forms.filter(|t| t.starts_with(word)) {
            found.push(candidate(
                text.clone(),
                text,
                option.description.as_deref(),
                true,
            ));
        }
    }
    found
}

/// The type of the value that follows the arguments `args` of a declared
/// command: that of the option before it when that takes a value, and
/// otherwise that of its position, or of the rest; `None` past them.
fn value_type(declared: &Extern, args: &[(usize, String)]) -> Option<ParamType> {
    let mut position = 0;
    let mut option_value = None;
    for (_, arg) in args {
        if option_value.take().is_some() {
            continue;
        }
        if arg.starts_with('-') && arg != "-" {
            option_value = declared.option_value(arg).cloned();
            continue;
        }
        position += 1;
    }
    if option_value.is_some() {
        return option_value;
    }
    declared.positional_type(position).cloned()
}

/// The candidates for a value of type `kind`: only folders for a
/// folder, files and folders for a file or path, commands for a command,
/// the choices of a list, and none for the other types.
fn by_type(
    kind: &ParamType,
    word: &str,
    sources: &dyn Sources,
    open: Option<char>,
) -> Vec<Candidate> {
    match kind {
        ParamType::Folder => files(word, sources, open)
            .into_iter()
            .filter(|c| !c.done)
            .collect(),
        ParamType::File | ParamType::Path | ParamType::Bool => files(word, sources, open),
        ParamType::Command => quote_names(commands(word, sources), open),
        ParamType::Choices(choices) => choices
            .iter()
            .filter(|c| c.starts_with(word))
            .map(|c| candidate(c.clone(), c.clone(), None, true))
            .collect(),
        _ => Vec::new(),
    }
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
/// whose names start as the rest does, hidden ones only for a rest that
/// starts with `.`, put on the line quoted as needed. The folder is
/// looked up as `expanded` gives it, and the word keeps its `~` and
/// variables; one that ends in them goes on with the `/` after them
/// when they name a folder.
fn files(word: &str, sources: &dyn Sources, open: Option<char>) -> Vec<Candidate> {
    let full = match open {
        Some('\'') => Some(word.to_owned()),
        _ => expanded(word, sources),
    };
    let Some(full) = full else {
        return Vec::new();
    };
    let at = word.rfind('/');
    let last = &word[at.map_or(0, |at| at + 1)..];
    if full != word && (last.contains('$') || word == "~") {
        return match sources.entries(&full) {
            Some(_) => vec![candidate(
                format!("{word}/"),
                format!("{last}/"),
                None,
                false,
            )],
            None => Vec::new(),
        };
    }
    let (folder, prefix) = match at {
        Some(at) => (&word[..=at], &word[at + 1..]),
        None => ("", word),
    };
    let full_folder = match full.rfind('/') {
        Some(at) => &full[..=at],
        None => "",
    };
    let listed = match full_folder {
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
            let named = format!("{name}{slash}");
            // What the typed folder expands stays as typed.
            let text = match folder == full_folder {
                true => quoted(&format!("{folder}{named}"), open, !folder_entry),
                false => format!("{folder}{}", quoted(&named, None, !folder_entry)),
            };
            candidate(text, named, None, !folder_entry)
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
    let kind = param.kind.written();
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
        .map(|p| p.kind.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    struct Fake;

    /// The declarations that `Fake` gives.
    const DECLARED: &str = r#"
extern git push(remote?: [origin upstream], ...refs: string, --force(-f): bool "overwrite", --repo: folder)
extern cd(folder?: folder)
extern print(--n(-n): bool, ...values: any)
extern pick(choice: [red green], target: file, run: command)
"#;

    fn declared() -> Vec<Extern> {
        parse(DECLARED)
            .unwrap()
            .statements
            .into_iter()
            .filter_map(|s| match s.kind {
                crate::ast::StatementKind::Extern(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    impl Sources for Fake {
        fn declaration(&self, name: &str, sub: Option<&str>) -> Option<Rc<Extern>> {
            declared()
                .into_iter()
                .find(|e| e.command == name && e.subcommand.as_deref() == sub)
                .map(Rc::new)
        }
        fn subcommands(&self, name: &str) -> Vec<String> {
            declared()
                .into_iter()
                .filter(|e| e.command == name)
                .filter_map(|e| e.subcommand)
                .collect()
        }
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
                ("here".into(), "src".into()),
                ("o".into(), "odd".into()),
            ]
        }
        fn environment(&self) -> Vec<(String, String)> {
            vec![
                ("PATH".into(), "/usr/bin".into()),
                ("PWD".into(), "/".into()),
                ("HOME".into(), "/".into()),
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
    fn completes_paths_after_expanding_them() {
        assert_eq!(texts("cd ~/U"), ["~/Users/"]);
        assert_eq!(texts("cat $here/m"), ["$here/main.rs", "$here/mod/"]);
        // A word that ends in what expands goes on with its `/`.
        assert_eq!(texts("cd ~"), ["~/"]);
        assert_eq!(texts("cd $env.HOME"), ["$env.HOME/"]);
        // Only the completed name is quoted.
        assert_eq!(texts("cd $o/M"), ["$o/'My Docs/"]);
        assert!(texts("cat $nope/x").is_empty());
        assert!(texts("cat $here.x/y").is_empty());
    }

    #[test]
    fn completes_from_declarations() {
        assert_eq!(texts("git p"), ["push"]);
        assert_eq!(texts("git push -"), ["--force", "--repo", "-f"]);
        let c = complete("git push --f", 12, &Fake);
        assert_eq!(c.candidates[0].description.as_deref(), Some("overwrite"));
        // Values by the type of their position, past options.
        assert_eq!(texts("git push o"), ["origin"]);
        assert_eq!(texts("git push --force o"), ["origin"]);
        assert!(texts("git push origin ").is_empty());
        // An option's value by the option's type.
        assert_eq!(texts("git push --repo "), ["src/"]);
        assert_eq!(texts("cd "), ["src/"]);
        assert_eq!(texts("cd -"), ["-"]);
        assert_eq!(texts("pick g"), ["green"]);
        assert_eq!(texts("pick red n"), ["notes.txt"]);
        assert_eq!(texts("pick red notes.txt ca"), ["cat"]);
        // Past what is declared, files.
        assert_eq!(texts("pick red notes.txt cat n"), ["notes.txt"]);
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
        assert_eq!(texts("print -"), ["--n", "-n"]);
        assert_eq!(texts("kill -T"), ["-TERM", "-TSTP"]);
        // A path option's value is a file; an int's has no candidates.
        assert_eq!(texts("./go.msh --out n"), ["notes.txt"]);
        assert!(texts("./go.msh --count ").is_empty());
        assert_eq!(texts("./go.msh --fast n"), ["notes.txt"]);
    }
}
