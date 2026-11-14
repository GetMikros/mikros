//! The colours of the line as it is typed. The line is read word by
//! word, as it stands while it is typed, which may not parse yet: a word
//! where a command goes is a command, plain when it is a keyword, a
//! built-in command, a function or a program that is found, and crimson
//! when it is not. Other words are arguments, mint green, and
//! underlined when they name a file or folder that exists; one written
//! as a path, from `/`, `~/`, `./` or `../`, that names nothing and that
//! no entry of its folder starts as is crimson, as is the argument of
//! `cd` that is no folder and that no folder starts as. Strings are
//! orange, variables teal, operators mint green, `|`, `;`, `&` and
//! redirections green, comments dark grey, and a closing bracket that
//! closes nothing crimson. Those are the defaults of `$msh_colors`, a
//! record that interactive msh defines before it runs `~/.mshrc` and
//! reads each time it draws the line, so that the colours can be
//! changed there or at the prompt.

use super::complete::Sources;
use crate::ast::{Extern, ParamType};
use crate::value::Value;
use std::collections::HashSet;
use std::rc::Rc;

/// How a part of the line is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Plain,
    /// A keyword, or a command that is found.
    Command,
    /// A command or path that is not found, or a syntax error.
    Error,
    Argument,
    /// An argument that names a file or folder that exists.
    Path,
    String,
    Variable,
    /// An operator of an expression or assignment.
    Operator,
    /// `|`, `;`, `&` or a redirection.
    Separator,
    Comment,
}

/// The fields of `$msh_colors`, with their defaults: a colour,
/// `normal`, one of the 16 names, or `#rrggbb`, and then `bold`,
/// `italic` or `underline`.
const FIELDS: [(&str, &str); 10] = [
    ("command", "normal"),
    ("error", "#e74c3c"),
    ("argument", "#1bd5b1"),
    ("path", "#1bd5b1 underline"),
    ("string", "#ff861a"),
    ("variable", "#15c1a0"),
    ("operator", "#1bd5b1"),
    ("separator", "#11d116"),
    ("comment", "brblack"),
    ("suggestion", "brblack"),
];
/// The 16 colours by name, with `br` in front for the bright ones.
const NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

/// `$msh_colors` with its defaults.
pub fn default_colors() -> Value {
    Value::Record(
        FIELDS
            .iter()
            .map(|(name, value)| (name.to_string(), Value::String(value.to_string())))
            .collect(),
    )
}

/// The escape sequence of the colour `spec`, if it is a valid one.
fn escape(spec: &str) -> Option<String> {
    let mut words = spec.split_whitespace();
    let colour = words.next()?;
    let mut codes = vec!["0".to_string()];
    for word in words {
        let code = match word {
            "bold" => "1",
            "italic" => "3",
            "underline" => "4",
            _ => return None,
        };
        codes.push(code.into());
    }
    match colour {
        "normal" => {}
        hex if hex.starts_with('#') => {
            let digits = &hex[1..];
            if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let n = u32::from_str_radix(digits, 16).ok()?;
            codes.push(format!("38;2;{};{};{}", n >> 16, n >> 8 & 255, n & 255));
        }
        name => {
            let (base, bright) = match name.strip_prefix("br") {
                Some(base) => (base, 90),
                None => (name, 30),
            };
            let index = NAMES.iter().position(|n| *n == base)?;
            codes.push((bright + index).to_string());
        }
    }
    Some(format!("\x1b[{}m", codes.join(";")))
}

/// The escape sequences that the styles are drawn with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    /// One for each of `FIELDS`.
    escapes: Vec<String>,
}

impl Default for Palette {
    fn default() -> Palette {
        Palette::new(None)
    }
}

impl Palette {
    /// The palette of `colors`, the value of `$msh_colors`: each field
    /// that is missing or not a valid colour takes its default.
    pub fn new(colors: Option<&Value>) -> Palette {
        let escapes = FIELDS
            .iter()
            .map(|(name, default)| {
                let set = match colors.map(|c| c.field(name)) {
                    Some(Ok(Value::String(spec))) => escape(spec),
                    _ => None,
                };
                set.or_else(|| escape(default))
                    .expect("the defaults are valid")
            })
            .collect();
        Palette { escapes }
    }

    /// The escape sequence that starts drawing in `style`.
    pub fn sgr(&self, style: Style) -> &str {
        let field = match style {
            Style::Plain => return "\x1b[0m",
            Style::Command => 0,
            Style::Error => 1,
            Style::Argument => 2,
            Style::Path => 3,
            Style::String => 4,
            Style::Variable => 5,
            Style::Operator => 6,
            Style::Separator => 7,
            Style::Comment => 8,
        };
        &self.escapes[field]
    }

    /// The escape sequence of the suggestion, menu descriptions and the
    /// search prompt.
    pub fn suggestion(&self) -> &str {
        &self.escapes[9]
    }
}

/// The keywords that start a statement, and those that go on one.
const KEYWORDS: [&str; 18] = [
    "let", "if", "else", "for", "in", "while", "match", "fn", "param", "try", "catch", "use",
    "return", "delete", "not", "and", "or", "extern",
];
/// The keywords after which a command goes.
const BEFORE_COMMAND: [&str; 7] = ["if", "else", "while", "not", "run", "exec", "return"];
/// The operators of assignments and comparisons, as words of their own.
const OPERATORS: [&str; 11] = [
    "=", "+=", "-=", "==", "!=", "<=", ">=", "=~", "!~", "??", "=>",
];
/// The operators of arithmetic, which are operators only in an
/// expression: elsewhere, `/`, `-` and `*` are usual arguments.
const ARITHMETIC: [&str; 6] = ["+", "-", "*", "/", "%", ".."];
/// The characters that end a bare word.
const SPECIAL: &str = "|;&><(){}[]";

/// Whether `word` is written as a path, from `/`, `~/`, `./` or `../`.
fn written_as_path(word: &str) -> bool {
    word == "~" || ["/", "~/", "./", "../"].iter().any(|p| word.starts_with(p))
}

/// Whether `word` is never taken for a path that must exist: an option,
/// a glob pattern, or another user's home folder as `~name`.
fn never_missing(word: &str) -> bool {
    word.starts_with('-')
        || word.contains(['*', '?'])
        || word.starts_with('~') && !(word == "~" || word.starts_with("~/"))
}

/// The words of `line` where commands go, apart from keywords, in order.
pub fn command_words(line: &str, sources: &dyn Sources) -> Vec<String> {
    reader(line, sources).commands
}

/// The style of each byte of `line`, with commands and files looked up
/// in `sources`.
pub fn styles(line: &str, sources: &dyn Sources) -> Vec<Style> {
    reader(line, sources).styles
}

/// `line`, read.
fn reader<'a>(line: &'a str, sources: &'a dyn Sources) -> Reader<'a> {
    let mut known: HashSet<String> = sources.programs().into_iter().collect();
    known.extend(sources.functions().into_iter().map(|(name, _)| name));
    known.extend(crate::eval::builtins::names().into_iter().map(String::from));
    let home = sources
        .environment()
        .into_iter()
        .find_map(|(name, value)| (name == "HOME").then_some(value));
    let mut reader = Reader {
        line,
        at: 0,
        styles: vec![Style::Plain; line.len()],
        known,
        sources,
        home,
        expression: false,
        target: false,
        arguments: None,
        declaring: false,
        commands: Vec::new(),
    };
    reader.read();
    reader
}

/// Where the arguments of a declared command are.
struct Arguments {
    name: String,
    declared: Option<Rc<Extern>>,
    /// Whether the next word may name a subcommand.
    first: bool,
    position: usize,
    /// The type of the value that the option before takes.
    option_value: Option<ParamType>,
}

struct Reader<'a> {
    line: &'a str,
    /// The byte being read.
    at: usize,
    styles: Vec<Style>,
    known: HashSet<String>,
    sources: &'a dyn Sources,
    /// The folder that `~` stands for.
    home: Option<String>,
    /// Whether the statement read is an expression, as its first word
    /// tells.
    expression: bool,
    /// Whether the next word is the target of `>`, `>>` or `2>`, which
    /// need not exist.
    target: bool,
    /// The arguments of the command read, when it is declared.
    arguments: Option<Arguments>,
    /// Whether the statement read is an `extern` declaration, whose words
    /// are no commands.
    declaring: bool,
    /// The words where commands go, apart from keywords.
    commands: Vec<String>,
}

impl Reader<'_> {
    fn peek(&self) -> Option<char> {
        self.line[self.at..].chars().next()
    }

    fn looking_at(&self, text: &str) -> bool {
        self.line[self.at..].starts_with(text)
    }

    fn paint(&mut self, from: usize, to: usize, style: Style) {
        for s in &mut self.styles[from..to] {
            *s = style;
        }
    }

    /// Takes `n` bytes in `style`.
    fn take(&mut self, n: usize, style: Style) {
        self.paint(self.at, self.at + n, style);
        self.at += n;
    }

    fn read(&mut self) {
        let mut command = true;
        let mut open: Vec<char> = Vec::new();
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\r' => self.at += 1,
                '\n' | ';' => {
                    self.take(1, Style::Separator);
                    command = true;
                    self.expression = false;
                    self.arguments = None;
                    self.declaring = false;
                }
                '#' => {
                    let end = self.line[self.at..]
                        .find('\n')
                        .map_or(self.line.len(), |n| self.at + n);
                    self.take(end - self.at, Style::Comment);
                }
                '|' | '&' => {
                    self.take(1, Style::Separator);
                    command = true;
                    self.expression = false;
                    self.arguments = None;
                }
                '>' | '<' => self.redirection(),
                '2' if self.looking_at("2>") => self.redirection(),
                '(' | '{' | '[' => {
                    open.push(c);
                    self.at += 1;
                    command = c != '[' && !self.declaring;
                    self.expression &= c == '[';
                }
                ')' | '}' | ']' => {
                    let opens = match c {
                        ')' => '(',
                        '}' => '{',
                        _ => '[',
                    };
                    let style = match open.pop() {
                        Some(o) if o == opens => Style::Plain,
                        _ => Style::Error,
                    };
                    self.take(1, style);
                    // `else` or `catch` may follow a block.
                    command = c == '}';
                }
                _ => {
                    let keyword = command && self.looking_at("extern");
                    command = self.word(command);
                    if keyword && !command {
                        self.declaring = true;
                    }
                }
            }
        }
    }

    /// `>`, `>>`, `<`, `2>`, `2>&1` or `>&2`.
    fn redirection(&mut self) {
        let found = ["2>&1", ">&2", "2>", ">>", ">", "<"]
            .into_iter()
            .find(|r| self.looking_at(r))
            .unwrap_or(">");
        self.target = matches!(found, "2>" | ">>" | ">");
        self.take(found.len(), Style::Separator);
    }

    /// A word, where a command goes when `command`, with its strings and
    /// variables; whether a command goes after it.
    fn word(&mut self, command: bool) -> bool {
        let start = self.at;
        let target = std::mem::take(&mut self.target);
        if command && self.starts_expression() {
            self.expression = true;
        }
        let mut bare = String::new();
        let mut plain = true;
        while let Some(c) = self.peek() {
            if c.is_whitespace() || SPECIAL.contains(c) {
                break;
            }
            match c {
                '\'' | '"' => {
                    self.string(c);
                    plain = false;
                }
                '$' => {
                    self.variable();
                    plain = false;
                }
                c => {
                    bare.push(c);
                    let from = self.at;
                    self.at += c.len_utf8();
                    self.paint(from, self.at, Style::Plain);
                }
            }
        }
        let end = self.at;
        let kind = match command || target || !plain {
            true => None,
            false => self.argument_type(&bare),
        };
        let operator = OPERATORS.contains(&bare.as_str())
            || self.expression && ARITHMETIC.contains(&bare.as_str());
        let style = if plain && operator {
            Style::Operator
        } else if command && plain {
            let style = self.command_style(&bare);
            if KEYWORDS.contains(&bare.as_str()) {
                self.arguments = None;
            } else if matches!(style, Style::Command | Style::Error) {
                self.commands.push(bare.clone());
                self.arguments = Some(Arguments {
                    declared: self.sources.declaration(&bare, None),
                    name: bare.clone(),
                    first: true,
                    position: 0,
                    option_value: None,
                });
            }
            style
        } else if !plain || never_missing(&bare) {
            Style::Argument
        } else if let Some(kind @ (ParamType::Folder | ParamType::File | ParamType::Path)) = &kind {
            // A declared position takes only what is of its kind.
            let fits = |is_folder: bool| match kind {
                ParamType::Folder => is_folder,
                ParamType::File => !is_folder,
                _ => true,
            };
            match self.file_kind(&bare) {
                Some(is_folder) if fits(is_folder) => Style::Path,
                _ if self.completes(&bare, &fits) => Style::Argument,
                _ => Style::Error,
            }
        } else if self.file_kind(&bare).is_some() {
            Style::Path
        } else if !target && written_as_path(&bare) && !self.completes(&bare, &|_| true) {
            Style::Error
        } else {
            Style::Argument
        };
        // The strings and variables in the word keep their colours.
        for s in &mut self.styles[start..end] {
            if *s == Style::Plain {
                *s = style;
            }
        }
        match style {
            Style::Operator => matches!(bare.as_str(), "=" | "+=" | "-="),
            Style::Command if BEFORE_COMMAND.contains(&bare.as_str()) => true,
            Style::Variable if command => true,
            _ => command && bare.contains('=') && plain,
        }
    }

    /// Whether the word here starts as an expression does, as the first
    /// word of a statement.
    fn starts_expression(&self) -> bool {
        let rest = &self.line[self.at..];
        let word = rest
            .split(|c: char| c.is_whitespace() || SPECIAL.contains(c))
            .next()
            .unwrap_or("");
        rest.starts_with(|c: char| c == '$' || c == '\'' || c == '"' || c.is_ascii_digit())
            || ["true", "false", "null", "not"].contains(&word)
    }

    /// The style of the word `name` where a command goes.
    fn command_style(&self, name: &str) -> Style {
        let rest = self.line[self.at..].trim_start_matches([' ', '\t']);
        let assigned = ["=", "+=", "-="]
            .iter()
            .any(|op| rest.starts_with(op) && !rest.starts_with("=="));
        if KEYWORDS.contains(&name) {
            Style::Command
        } else if assigned || name.contains('=') && !name.starts_with('=') {
            // An assignment's target, or a NAME=value word.
            Style::Variable
        } else if name.starts_with(|c: char| c.is_ascii_digit())
            || ["true", "false", "null"].contains(&name)
        {
            Style::Argument
        } else if self.known.contains(name)
            || name.contains('/') && self.file_kind(name) == Some(false)
        {
            Style::Command
        } else {
            Style::Error
        }
    }

    /// `path` with `~` and `~/` at its start as the home folder, or
    /// `None` when the home folder is not known.
    fn expanded(&self, path: &str) -> Option<String> {
        match path.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                Some(format!("{}{rest}", self.home.as_deref()?))
            }
            _ => Some(path.to_owned()),
        }
    }

    /// The declared type of the argument `word` of the command read, as
    /// its position or the option before it gives it; a word that names a
    /// declared subcommand turns to that subcommand's declaration.
    fn argument_type(&mut self, word: &str) -> Option<ParamType> {
        let arguments = self.arguments.as_mut()?;
        if std::mem::take(&mut arguments.first)
            && let Some(declared) = self.sources.declaration(&arguments.name, Some(word))
        {
            arguments.declared = Some(declared);
            return None;
        }
        let declared = arguments.declared.as_ref()?;
        if let Some(kind) = arguments.option_value.take() {
            return Some(kind);
        }
        if word.starts_with('-') && word != "-" {
            arguments.option_value = declared.option_value(word).cloned();
            return None;
        }
        arguments.position += 1;
        declared.positional_type(arguments.position - 1).cloned()
    }

    /// Whether an entry of the folder of `path`, one that `fits` by
    /// whether it is a folder, starts with its last part, so that
    /// completion can still make it a name.
    fn completes(&self, path: &str, fits: &dyn Fn(bool) -> bool) -> bool {
        let Some(path) = self.expanded(path) else {
            return false;
        };
        let (folder, prefix) = match path.rfind('/') {
            Some(0) => ("/", &path[1..]),
            Some(at) => (&path[..at], &path[at + 1..]),
            None => (".", path.as_str()),
        };
        !prefix.is_empty()
            && self.sources.entries(folder).is_some_and(|entries| {
                entries
                    .iter()
                    .any(|(name, is_folder)| name.starts_with(prefix) && fits(*is_folder))
            })
    }

    /// Whether `path` is a folder, if it names a file or folder that
    /// exists, with `~` and `~/` at its start as the home folder.
    fn file_kind(&self, path: &str) -> Option<bool> {
        let path = self.expanded(path)?;
        let trimmed = path.trim_end_matches('/');
        if trimmed.is_empty() {
            return path.starts_with('/').then_some(true);
        }
        let (folder, name) = match trimmed.rfind('/') {
            Some(0) => ("/", &trimmed[1..]),
            Some(at) => (&trimmed[..at], &trimmed[at + 1..]),
            None => (".", trimmed),
        };
        if name == "." || name == ".." {
            return Some(true);
        }
        let entries = self.sources.entries(folder)?;
        let (_, is_folder) = entries.into_iter().find(|(n, _)| n == name)?;
        (is_folder || !path.ends_with('/')).then_some(is_folder)
    }

    /// A quoted string, with the `$name`s in a double-quoted one.
    fn string(&mut self, quote: char) {
        self.take(1, Style::String);
        while let Some(c) = self.peek() {
            match c {
                c if c == quote => {
                    self.take(1, Style::String);
                    return;
                }
                '\\' if quote == '"' => {
                    let n = 1 + self.line[self.at + 1..]
                        .chars()
                        .next()
                        .map_or(0, char::len_utf8);
                    self.take(n, Style::String);
                }
                '$' if quote == '"' => self.variable(),
                c => self.take(c.len_utf8(), Style::String),
            }
        }
    }

    /// `$`, and the name and path that follow it.
    fn variable(&mut self) {
        let start = self.at;
        self.at += 1;
        let name = |c: char| c.is_ascii_alphanumeric() || c == '_';
        loop {
            let rest = &self.line[self.at..];
            let length = rest.find(|c: char| !name(c)).unwrap_or(rest.len());
            self.at += length;
            let rest = &self.line[self.at..];
            let field = rest
                .strip_prefix('.')
                .and_then(|r| r.chars().next())
                .is_some_and(name);
            if !field || rest.starts_with("...") {
                break;
            }
            self.at += 1;
        }
        if self.looking_at("...") {
            self.at += 3;
        }
        self.paint(start, self.at, Style::Variable);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Param;

    struct Fake;

    impl Sources for Fake {
        fn entries(&self, folder: &str) -> Option<Vec<(String, bool)>> {
            let entries: &[(&str, bool)] = match folder {
                "." => &[("notes.txt", false), ("src", true), ("run.sh", false)],
                "/" => &[("usr", true)],
                _ => return None,
            };
            Some(entries.iter().map(|(n, f)| (n.to_string(), *f)).collect())
        }
        fn programs(&self) -> Vec<String> {
            vec!["ls".into(), "cat".into()]
        }
        fn functions(&self) -> Vec<(String, Vec<String>)> {
            vec![("deploy".into(), Vec::new())]
        }
        fn variables(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn environment(&self) -> Vec<(String, String)> {
            vec![("HOME".into(), "/".into())]
        }
        fn params(&self, _: &str) -> Option<Vec<Param>> {
            None
        }
        fn declaration(&self, name: &str, sub: Option<&str>) -> Option<Rc<Extern>> {
            let declared = format!(
                "{}\nextern pick(target: file, ...rest: path)\nextern git push(--repo: folder)",
                crate::eval::builtins::DECLARATIONS
            );
            crate::parse::parse(&declared)
                .unwrap()
                .statements
                .into_iter()
                .find_map(|s| match s.kind {
                    crate::ast::StatementKind::Extern(e)
                        if e.command == name && e.subcommand.as_deref() == sub =>
                    {
                        Some(Rc::new(e))
                    }
                    _ => None,
                })
        }
    }

    /// One letter for each byte's style.
    fn shown(line: &str) -> String {
        styles(line, &Fake)
            .iter()
            .map(|s| match s {
                Style::Plain => '.',
                Style::Command => 'c',
                Style::Error => 'E',
                Style::Argument => 'a',
                Style::Path => 'p',
                Style::String => 's',
                Style::Variable => 'v',
                Style::Operator => 'o',
                Style::Separator => '|',
                Style::Comment => '#',
            })
            .collect()
    }

    #[test]
    fn takes_colours_from_msh_colors_with_defaults() {
        let palette = Palette::default();
        assert_eq!(palette.sgr(Style::Command), "\x1b[0m");
        assert_eq!(palette.sgr(Style::Path), "\x1b[0;4;38;2;27;213;177m");
        assert_eq!(palette.suggestion(), "\x1b[0;90m");
        let set = |fields: &[(&str, &str)]| {
            let record = Value::Record(
                fields
                    .iter()
                    .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
                    .collect(),
            );
            Palette::new(Some(&record))
        };
        let p = set(&[
            ("string", "brred bold"),
            ("command", "#0000ff italic underline"),
            ("error", "pink"),
            ("comment", "#12345"),
        ]);
        assert_eq!(p.sgr(Style::String), "\x1b[0;1;91m");
        assert_eq!(p.sgr(Style::Command), "\x1b[0;3;4;38;2;0;0;255m");
        // Invalid values and missing fields keep their defaults.
        assert_eq!(p.sgr(Style::Error), palette.sgr(Style::Error));
        assert_eq!(p.sgr(Style::Comment), "\x1b[0;90m");
        assert_eq!(p.sgr(Style::Argument), palette.sgr(Style::Argument));
        assert_eq!(Palette::new(Some(&Value::Int(1))), palette);
        assert_eq!(Palette::new(Some(&default_colors())), palette);
    }

    #[test]
    fn colours_commands_arguments_and_files() {
        assert_eq!(shown("ls -l notes.txt nope"), "cc.aa.ppppppppp.aaaa");
        assert_eq!(shown("nope src/ /usr"), "EEEE.pppp.pppp");
        assert_eq!(shown("deploy | cat; print"), "cccccc.|.ccc|.ccccc");
        assert_eq!(shown("./run.sh &"), "cccccccc.|");
        assert_eq!(shown("./gone.sh"), "EEEEEEEEE");
    }

    #[test]
    fn colours_paths_that_name_nothing_as_errors() {
        // Only words written as paths; globs and other words stay.
        assert_eq!(
            shown("cat ./gone /nope origin/main src/*.rs"),
            "ccc.EEEEEE.EEEEE.aaaaaaaaaaa.aaaaaaaa"
        );
        assert_eq!(
            shown("ls ~ ~/usr ~/nope ~bob -x"),
            "cc.p.ppppp.EEEEEE.aaaa.aa"
        );
        // The argument of cd must be a folder.
        assert_eq!(
            shown("cd nope; cd notes.txt; cd src"),
            "cc.EEEE|.cc.EEEEEEEEE|.cc.ppp"
        );
        // Declared positions take only what is of their kind, past options
        // and in subcommands.
        assert_eq!(shown("pick src notes.txt nope"), "EEEE.EEE.ppppppppp.EEEE");
        assert_eq!(shown("pick no"), "EEEE.aa");
        assert_eq!(
            shown("git push --repo src --repo nope"),
            "EEE.aaaa.aaaaaa.ppp.aaaaaa.EEEE"
        );
        // A redirection's target need not exist.
        assert_eq!(shown("ls > ./new.txt"), "cc.|.aaaaaaaaa");
        // A path that an entry still completes is no error yet.
        assert_eq!(shown("cat ./no ./sr /u ./zz"), "ccc.aaaa.aaaa.aa.EEEE");
        assert_eq!(shown("cd sr; cd no"), "cc.aa|.cc.EE");
    }

    #[test]
    fn colours_strings_variables_operators_and_comments() {
        assert_eq!(shown("echo 'a b' \"x $y\" # c"), "EEEE.sssss.sssvvs.###");
        assert_eq!(shown("cat $r.a.0... > f"), "ccc.vvvvvvvvv.|.a");
        assert_eq!(shown("ls 2>&1 >> log"), "cc.||||.||.aaa");
    }

    #[test]
    fn colours_keywords_assignments_and_right_sides() {
        assert_eq!(shown("if $x == 1 { ls }"), "cc.vv.oo.a...cc..");
        assert_eq!(shown("let n = ls | len"), "ccc.a.o.cc.|.ccc");
        assert_eq!(shown("n += 2"), "v.oo.a");
        assert_eq!(shown("A=1 ls"), "vvv.cc");
        // Arithmetic is an operator only in an expression.
        assert_eq!(shown("let n = 1 + -2"), "ccc.a.o.a.o.aa");
        assert_eq!(shown("cd / -"), "cc.p.a");
        assert_eq!(shown("ls * | $x * 2"), "cc.a.|.vv.o.a");
        assert_eq!(shown("} else { cat }"), "E.cccc...ccc..");
        // The words of a declaration are no commands.
        assert_eq!(
            shown("extern git push(--force: bool)"),
            "cccccc.aaa.aaaa.aaaaaaaa.aaaa."
        );
    }

    #[test]
    fn finds_the_words_where_commands_go() {
        assert_eq!(
            command_words("ls -l | git push; A=1 cat x; if true { make }", &Fake),
            ["ls", "git", "cat", "make"]
        );
    }

    #[test]
    fn colours_a_bracket_that_closes_nothing_as_an_error() {
        assert_eq!(shown("ls )"), "cc.E");
        assert_eq!(shown("(ls)"), ".cc.");
        assert_eq!(shown("[a b]"), ".a.a.");
    }
}
