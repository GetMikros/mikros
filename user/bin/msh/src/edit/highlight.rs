//! The colours of the line as it is typed. The line is read word by
//! word, as it stands while it is typed, which may not parse yet: a word
//! where a command goes is a command, blue when it is a keyword, a
//! built-in command, a function or a program that is found, and bright
//! red when it is not; other words are arguments, cyan, and underlined
//! when they name a file or folder that exists. Strings are yellow,
//! variables, operators, `|`, `;`, `&` and redirections bright cyan,
//! comments dark grey, and a closing bracket that closes nothing bright
//! red.

use super::complete::Sources;
use std::collections::HashSet;

/// How a part of the line is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Plain,
    /// A keyword, or a command that is found.
    Command,
    /// A command that is not found, or a syntax error.
    Error,
    Argument,
    /// An argument that names a file or folder that exists.
    Path,
    String,
    Variable,
    Operator,
    Comment,
}

impl Style {
    /// The escape sequence that starts drawing in the style.
    pub fn sgr(self) -> &'static str {
        match self {
            Style::Plain => "\x1b[0m",
            Style::Command => "\x1b[0;34m",
            Style::Error => "\x1b[0;91m",
            Style::Argument => "\x1b[0;36m",
            Style::Path => "\x1b[0;36;4m",
            Style::String => "\x1b[0;33m",
            Style::Variable | Style::Operator => "\x1b[0;96m",
            Style::Comment => "\x1b[0;90m",
        }
    }
}

/// The keywords that start a statement, and those that go on one.
const KEYWORDS: [&str; 17] = [
    "let", "if", "else", "for", "in", "while", "match", "fn", "param", "try", "catch", "use",
    "return", "delete", "not", "and", "or",
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

/// The style of each byte of `line`, with commands and files looked up
/// in `sources`.
pub fn styles(line: &str, sources: &dyn Sources) -> Vec<Style> {
    let mut known: HashSet<String> = sources.programs().into_iter().collect();
    known.extend(sources.functions().into_iter().map(|(name, _)| name));
    known.extend(crate::eval::builtins::names().into_iter().map(String::from));
    let mut reader = Reader {
        line,
        at: 0,
        styles: vec![Style::Plain; line.len()],
        known,
        sources,
        expression: false,
    };
    reader.read();
    reader.styles
}

struct Reader<'a> {
    line: &'a str,
    /// The byte being read.
    at: usize,
    styles: Vec<Style>,
    known: HashSet<String>,
    sources: &'a dyn Sources,
    /// Whether the statement read is an expression, as its first word
    /// tells.
    expression: bool,
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
                    self.take(1, Style::Operator);
                    command = true;
                    self.expression = false;
                }
                '#' => {
                    let end = self.line[self.at..]
                        .find('\n')
                        .map_or(self.line.len(), |n| self.at + n);
                    self.take(end - self.at, Style::Comment);
                }
                '|' | '&' => {
                    self.take(1, Style::Operator);
                    command = true;
                    self.expression = false;
                }
                '>' | '<' => self.redirection(),
                '2' if self.looking_at("2>") => self.redirection(),
                '(' | '{' | '[' => {
                    open.push(c);
                    self.at += 1;
                    command = c != '[';
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
                _ => command = self.word(command),
            }
        }
    }

    /// `>`, `>>`, `<`, `2>`, `2>&1` or `>&2`.
    fn redirection(&mut self) {
        let length = ["2>&1", ">&2", "2>", ">>", ">", "<"]
            .iter()
            .find(|r| self.looking_at(r))
            .map_or(1, |r| r.len());
        self.take(length, Style::Operator);
    }

    /// A word, where a command goes when `command`, with its strings and
    /// variables; whether a command goes after it.
    fn word(&mut self, command: bool) -> bool {
        let start = self.at;
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
        let operator = OPERATORS.contains(&bare.as_str())
            || self.expression && ARITHMETIC.contains(&bare.as_str());
        let style = if plain && operator {
            Style::Operator
        } else if command && plain {
            self.command_style(&bare)
        } else if plain && self.names_a_file(&bare) {
            Style::Path
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

    /// Whether `word` names a file or folder that exists.
    fn names_a_file(&self, word: &str) -> bool {
        !word.starts_with('-') && self.file_kind(word).is_some()
    }

    /// Whether `path` is a folder, if it names a file or folder that
    /// exists.
    fn file_kind(&self, path: &str) -> Option<bool> {
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
            Vec::new()
        }
        fn params(&self, _: &str) -> Option<Vec<Param>> {
            None
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
                Style::Comment => '#',
            })
            .collect()
    }

    #[test]
    fn colours_commands_arguments_and_files() {
        assert_eq!(shown("ls -l notes.txt nope"), "cc.aa.ppppppppp.aaaa");
        assert_eq!(shown("nope src/ /usr"), "EEEE.pppp.pppp");
        assert_eq!(shown("deploy | cat; print"), "cccccc.o.ccco.ccccc");
        assert_eq!(shown("./run.sh &"), "cccccccc.o");
        assert_eq!(shown("./gone.sh"), "EEEEEEEEE");
    }

    #[test]
    fn colours_strings_variables_operators_and_comments() {
        assert_eq!(shown("echo 'a b' \"x $y\" # c"), "EEEE.sssss.sssvvs.###");
        assert_eq!(shown("cat $r.a.0... > f"), "ccc.vvvvvvvvv.o.a");
        assert_eq!(shown("ls 2>&1 >> log"), "cc.oooo.oo.aaa");
    }

    #[test]
    fn colours_keywords_assignments_and_right_sides() {
        assert_eq!(shown("if $x == 1 { ls }"), "cc.vv.oo.a...cc..");
        assert_eq!(shown("let n = ls | len"), "ccc.a.o.cc.o.ccc");
        assert_eq!(shown("n += 2"), "v.oo.a");
        assert_eq!(shown("A=1 ls"), "vvv.cc");
        // Arithmetic is an operator only in an expression.
        assert_eq!(shown("let n = 1 + -2"), "ccc.a.o.a.o.aa");
        assert_eq!(shown("cd / -"), "cc.p.a");
        assert_eq!(shown("ls * | $x * 2"), "cc.a.o.vv.o.a");
        assert_eq!(shown("} else { cat }"), "E.cccc...ccc..");
    }

    #[test]
    fn colours_a_bracket_that_closes_nothing_as_an_error() {
        assert_eq!(shown("ls )"), "cc.E");
        assert_eq!(shown("(ls)"), ".cc.");
        assert_eq!(shown("[a b]"), ".a.a.");
    }
}
