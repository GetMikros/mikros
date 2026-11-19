//! Patterns as grep takes them: basic and extended regular expressions,
//! rewritten in the syntax of the regex crate, and fixed strings, and the
//! matcher of lines that joins them (MIKSPEC-0098.R52).

use regex::bytes::{Regex, RegexBuilder};

/// How a pattern is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syntax {
    Basic,
    Extended,
    Fixed,
}

/// A piece of a translated expression.
#[derive(Debug)]
struct Atom {
    text: String,
    /// Whether a repetition may follow it: not an anchor, a boundary or
    /// an alternation.
    repeatable: bool,
    /// Whether it has a repetition already, so that another one needs a
    /// group around it.
    repeated: bool,
    /// Whether it is the `|` between alternatives.
    bar: bool,
}

/// The translation so far: a list of atoms for the whole expression and
/// for each group that is open.
struct Builder {
    frames: Vec<Vec<Atom>>,
}

impl Builder {
    fn atoms(&mut self) -> &mut Vec<Atom> {
        self.frames.last_mut().expect("a frame is always open")
    }

    fn last(&self) -> Option<&Atom> {
        self.frames.last().and_then(|f| f.last())
    }

    fn push(&mut self, text: impl Into<String>, repeatable: bool) {
        self.atoms().push(Atom {
            text: text.into(),
            repeatable,
            repeated: false,
            bar: false,
        });
    }

    fn literal(&mut self, c: char) {
        self.push(regex::escape(c.encode_utf8(&mut [0; 4])), true);
    }

    /// Whether a repetition here has nothing to repeat, and is a literal.
    fn nothing_to_repeat(&self) -> bool {
        !self.last().is_some_and(|a| a.repeatable)
    }

    /// Whether this is the start of an alternative.
    fn branch_starts(&self) -> bool {
        self.last().is_none_or(|a| a.bar)
    }

    fn repeat(&mut self, repetition: &str) {
        let atom = self.atoms().last_mut().expect("checked before");
        atom.text = match atom.repeated {
            false => format!("{}{repetition}", atom.text),
            true => format!("(?:{}){repetition}", atom.text),
        };
        atom.repeated = true;
    }

    fn bar(&mut self) {
        self.atoms().push(Atom {
            text: "|".into(),
            repeatable: false,
            repeated: false,
            bar: true,
        });
    }

    fn open(&mut self) {
        self.frames.push(Vec::new());
    }

    fn close(&mut self) -> Result<(), String> {
        if self.frames.len() < 2 {
            return Err("unmatched ) or \\)".into());
        }
        let atoms = self.frames.pop().expect("checked");
        let text = format!("({})", join(&atoms));
        self.push(text, true);
        Ok(())
    }

    fn finish(self) -> Result<String, String> {
        match self.frames.len() {
            1 => Ok(join(&self.frames[0])),
            _ => Err("unmatched ( or \\(".into()),
        }
    }
}

fn join(atoms: &[Atom]) -> String {
    atoms.iter().map(|a| a.text.as_str()).collect()
}

/// The character classes of POSIX that bracket expressions take.
const CLASSES: [&str; 12] = [
    "alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print", "punct", "space",
    "upper", "xdigit",
];

/// `pattern`, written in `syntax`, in the syntax of the regex crate, or
/// why it cannot be.
pub fn translate(pattern: &str, syntax: Syntax) -> Result<String, String> {
    if syntax == Syntax::Fixed {
        return Ok(regex::escape(pattern));
    }
    let extended = syntax == Syntax::Extended;
    let chars: Vec<char> = pattern.chars().collect();
    let mut b = Builder {
        frames: vec![Vec::new()],
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '\\' => {
                let Some(&e) = chars.get(i) else {
                    return Err("trailing backslash".into());
                };
                i += 1;
                match e {
                    '(' if !extended => b.open(),
                    ')' if !extended => b.close()?,
                    '|' if !extended => b.bar(),
                    '{' if !extended => {
                        if b.nothing_to_repeat() {
                            return Err("nothing to repeat before \\{".into());
                        }
                        let (repetition, next) =
                            interval(&chars, i, false).ok_or("invalid interval \\{...\\}")??;
                        i = next;
                        b.repeat(&repetition);
                    }
                    '+' | '?' if !extended => match b.nothing_to_repeat() {
                        true => b.literal(e),
                        false => b.repeat(&e.to_string()),
                    },
                    '1'..='9' => return Err("back-references are not supported".into()),
                    '<' => b.push(r"\b{start}", false),
                    '>' => b.push(r"\b{end}", false),
                    'b' | 'B' => b.push(format!("\\{e}"), false),
                    'w' | 'W' | 's' | 'S' => b.push(format!("\\{e}"), true),
                    '`' => b.push(r"\A", false),
                    '\'' => b.push(r"\z", false),
                    other => b.literal(other),
                }
            }
            '[' => {
                let (class, next) = bracket(&chars, i)?;
                i = next;
                b.push(class, true);
            }
            '.' => b.push(".", true),
            '*' => match b.nothing_to_repeat() {
                true => b.literal('*'),
                false => b.repeat("*"),
            },
            '^' if extended || b.branch_starts() => b.push("^", false),
            '$' if extended || branch_ends(&chars, i) => b.push("$", false),
            '(' if extended => b.open(),
            ')' if extended => b.close()?,
            '|' if extended => b.bar(),
            '+' | '?' if extended => match b.nothing_to_repeat() {
                true => b.literal(c),
                false => b.repeat(&c.to_string()),
            },
            '{' if extended && !b.nothing_to_repeat() => match interval(&chars, i, true) {
                Some(Ok((repetition, next))) => {
                    i = next;
                    b.repeat(&repetition);
                }
                Some(Err(why)) => return Err(why),
                None => b.literal('{'),
            },
            other => b.literal(other),
        }
    }
    b.finish()
}

/// Whether a `$` of a basic expression, before `next`, ends an
/// alternative: at the end, or before `\)` or `\|`.
fn branch_ends(chars: &[char], next: usize) -> bool {
    match chars.get(next) {
        None => true,
        Some('\\') => matches!(chars.get(next + 1), Some(')' | '|')),
        Some(_) => false,
    }
}

/// The repetition of an interval whose `{` is before `at`, `{m}`,
/// `{m,}`, `{,n}` or `{m,n}` ending in `}` when `extended` and `\}`
/// otherwise, and where it ends; `None` when it is not an interval.
fn interval(chars: &[char], at: usize, extended: bool) -> Option<Result<(String, usize), String>> {
    let mut i = at;
    let number = |i: &mut usize| {
        let start = *i;
        while chars.get(*i).is_some_and(char::is_ascii_digit) {
            *i += 1;
        }
        chars[start..*i].iter().collect::<String>()
    };
    let least = number(&mut i);
    let comma = chars.get(i) == Some(&',');
    if comma {
        i += 1;
    }
    let most = match comma {
        true => number(&mut i),
        false => least.clone(),
    };
    match extended {
        true if chars.get(i) == Some(&'}') => i += 1,
        false if chars.get(i) == Some(&'\\') && chars.get(i + 1) == Some(&'}') => i += 2,
        _ => return None,
    }
    if least.is_empty() && most.is_empty() && !comma {
        return None;
    }
    let low: u32 = match least.is_empty() {
        true => 0,
        false => match least.parse() {
            Ok(n) => n,
            Err(_) => return Some(Err("interval too large".into())),
        },
    };
    let repetition = match (comma, most.is_empty()) {
        (false, _) => format!("{{{low}}}"),
        (true, true) => format!("{{{low},}}"),
        (true, false) => match most.parse::<u32>() {
            Ok(high) if high >= low => format!("{{{low},{high}}}"),
            Ok(_) => return Some(Err("invalid interval: the least is above the most".into())),
            Err(_) => return Some(Err("interval too large".into())),
        },
    };
    Some(Ok((repetition, i)))
}

/// `c` as a member of a class of the regex crate, which gives some
/// punctuation a meaning there.
fn class_member(c: char) -> String {
    match c {
        '\\' | '[' | ']' | '^' | '-' | '&' | '~' => format!("\\{c}"),
        c => c.to_string(),
    }
}

/// The bracket expression whose `[` is before `at`, as a class of the
/// regex crate, and where it ends.
fn bracket(chars: &[char], at: usize) -> Result<(String, usize), String> {
    let unmatched = || "unmatched [".to_string();
    let mut i = at;
    let mut out = String::from("[");
    if chars.get(i) == Some(&'^') {
        out.push('^');
        i += 1;
    }
    let mut first = true;
    loop {
        let &c = chars.get(i).ok_or_else(unmatched)?;
        if c == ']' && !first {
            out.push(']');
            return Ok((out, i + 1));
        }
        first = false;
        let start = match element(chars, &mut i)? {
            Element::Class(name) => {
                out.push_str(&format!("[:{name}:]"));
                continue;
            }
            Element::Char(c) => c,
        };
        let range = chars.get(i) == Some(&'-') && chars.get(i + 1).is_some_and(|&n| n != ']');
        if !range {
            out.push_str(&class_member(start));
            continue;
        }
        i += 1;
        let Element::Char(end) = element(chars, &mut i)? else {
            return Err("a character class cannot end a range".into());
        };
        if end < start {
            return Err(format!("invalid range {start}-{end}"));
        }
        out.push_str(&class_member(start));
        out.push('-');
        out.push_str(&class_member(end));
    }
}

enum Element {
    Char(char),
    Class(String),
}

/// The element of a bracket expression at `i`, after which `i` is left:
/// a character, `[:class:]`, or `[=c=]` and `[.c.]` of one character.
fn element(chars: &[char], i: &mut usize) -> Result<Element, String> {
    let &c = chars.get(*i).ok_or("unmatched [")?;
    let kind = chars.get(*i + 1).copied();
    if c != '[' || !matches!(kind, Some(':' | '=' | '.')) {
        *i += 1;
        return Ok(Element::Char(c));
    }
    let kind = kind.expect("matched");
    let close = (*i + 2..chars.len().saturating_sub(1))
        .find(|&j| chars[j] == kind && chars[j + 1] == ']')
        .ok_or("unmatched [")?;
    let name: String = chars[*i + 2..close].iter().collect();
    *i = close + 2;
    if kind == ':' {
        return match CLASSES.contains(&name.as_str()) {
            true => Ok(Element::Class(name)),
            false => Err(format!("invalid character class {name}")),
        };
    }
    let mut each = name.chars();
    match (each.next(), each.next()) {
        (Some(c), None) => Ok(Element::Char(c)),
        _ => Err(format!(
            "[{kind}{name}{kind}] is not supported: only single characters are"
        )),
    }
}

/// How lines are matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub syntax: Syntax,
    pub ignore_case: bool,
    /// Matches only whole words.
    pub word: bool,
    /// Matches only whole lines.
    pub line: bool,
    /// Whether lines end with NUL, and may hold line feeds that `.`
    /// matches.
    pub null_data: bool,
}

/// Matches lines against all the patterns.
#[derive(Debug)]
pub struct Matcher {
    /// None when there are no patterns, which match nothing.
    regex: Option<Regex>,
    /// Whether the match is the first group, between non-word
    /// characters.
    word: bool,
}

impl Matcher {
    pub fn new(patterns: &[String], settings: Settings) -> Result<Matcher, String> {
        if patterns.is_empty() {
            return Ok(Matcher {
                regex: None,
                word: false,
            });
        }
        let mut parts = Vec::new();
        for pattern in patterns {
            parts.push(format!("(?:{})", translate(pattern, settings.syntax)?));
        }
        let joined = parts.join("|");
        let word = settings.word && !settings.line;
        let full = if settings.line {
            format!("^(?:{joined})$")
        } else if word {
            format!(r"(?:^|\W)({joined})(?:\W|$)")
        } else {
            joined
        };
        let regex = RegexBuilder::new(&full)
            .case_insensitive(settings.ignore_case)
            .dot_matches_new_line(settings.null_data)
            .build()
            .map_err(|e| {
                let text = e.to_string();
                let why = text
                    .lines()
                    .rev()
                    .find_map(|l| l.strip_prefix("error: "))
                    .unwrap_or(&text)
                    .to_string();
                format!("invalid pattern: {why}")
            })?;
        Ok(Matcher {
            regex: Some(regex),
            word,
        })
    }

    pub fn is_match(&self, line: &[u8]) -> bool {
        self.regex.as_ref().is_some_and(|r| r.is_match(line))
    }

    /// The starts and ends of the matches in `line` that are not empty.
    pub fn matches(&self, line: &[u8]) -> Vec<(usize, usize)> {
        let Some(regex) = &self.regex else {
            return Vec::new();
        };
        if !self.word {
            return regex
                .find_iter(line)
                .filter(|m| !m.is_empty())
                .map(|m| (m.start(), m.end()))
                .collect();
        }
        let mut found = Vec::new();
        let mut at = 0;
        while at <= line.len() {
            let Some(group) = regex.captures_at(line, at).and_then(|c| c.get(1)) else {
                break;
            };
            if !group.is_empty() {
                found.push((group.start(), group.end()));
            }
            at = match group.end() > at {
                true => group.end(),
                false => at + 1,
            };
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic(p: &str) -> String {
        translate(p, Syntax::Basic).unwrap()
    }

    fn extended(p: &str) -> String {
        translate(p, Syntax::Extended).unwrap()
    }

    #[test]
    fn translates_basic_expressions() {
        assert_eq!(basic("a.b*"), "a.b*");
        assert_eq!(basic(r"\(ab\)*c"), "(ab)*c");
        assert_eq!(basic(r"a\{2,3\}"), "a{2,3}");
        assert_eq!(basic(r"a\{,3\}b\{2\}c\{1,\}"), "a{0,3}b{2}c{1,}");
        assert_eq!(basic("a+b?c|d(e){f}"), r"a\+b\?c\|d\(e\)\{f\}");
        assert_eq!(basic(r"a\+b\?c\|d"), "a+b?c|d");
        assert_eq!(basic("*a"), r"\*a");
        assert_eq!(basic(r"\(*a\)"), r"(\*a)");
        assert_eq!(basic("^*"), r"^\*");
        assert_eq!(basic("a^b$c"), r"a\^b\$c");
        assert_eq!(basic(r"^a$\|^b$"), "^a$|^b$");
        assert_eq!(basic(r"\.\*\[\\"), r"\.\*\[\\");
        assert_eq!(basic(r"\<w\>\bx\w"), r"\b{start}w\b{end}\bx\w");
        assert_eq!(basic("a**"), "(?:a*)*");
    }

    #[test]
    fn translates_extended_expressions() {
        assert_eq!(extended("(ab)+|c?d{2}"), "(ab)+|c?d{2}");
        assert_eq!(extended(r"a\{b\}\(\|"), r"a\{b\}\(\|");
        assert_eq!(extended("a{x"), r"a\{x");
        assert_eq!(extended("a{,2}"), "a{0,2}");
        assert_eq!(extended("*a|+b"), r"\*a|\+b");
        assert_eq!(extended("a^b$"), "a^b$");
        assert_eq!(extended("a+?"), "(?:a+)?");
    }

    #[test]
    fn translates_bracket_expressions() {
        assert_eq!(basic("[abc]"), "[abc]");
        assert_eq!(basic("[^a-z]"), "[^a-z]");
        assert_eq!(basic("[]a]"), r"[\]a]");
        assert_eq!(basic("[^]a]"), r"[^\]a]");
        assert_eq!(basic("[a-]"), r"[a\-]");
        assert_eq!(basic(r"[\[&~^]"), r"[\\\[\&\~\^]");
        assert_eq!(basic("[[:digit:]x]"), "[[:digit:]x]");
        assert_eq!(basic("[[=e=][.-.]]"), r"[e\-]");
        assert_eq!(extended("[*+?]+"), "[*+?]+");
    }

    #[test]
    fn refuses_what_it_cannot_translate() {
        let refused = |p: &str, s| translate(p, s).unwrap_err();
        assert_eq!(
            refused(r"\(a\)\1", Syntax::Basic),
            "back-references are not supported"
        );
        assert_eq!(
            refused(r"(a)\1", Syntax::Extended),
            "back-references are not supported"
        );
        assert_eq!(refused("[a", Syntax::Basic), "unmatched [");
        assert_eq!(refused(r"\(a", Syntax::Basic), "unmatched ( or \\(");
        assert_eq!(refused("a)", Syntax::Extended), "unmatched ) or \\)");
        assert_eq!(refused("a\\", Syntax::Basic), "trailing backslash");
        assert_eq!(
            refused("[[:nope:]]", Syntax::Basic),
            "invalid character class nope"
        );
        assert_eq!(refused("[z-a]", Syntax::Basic), "invalid range z-a");
        assert!(refused("[[.ab.]]", Syntax::Basic).contains("not supported"));
        assert!(refused(r"a\{3,1\}", Syntax::Basic).contains("interval"));
        assert!(refused(r"\{1\}", Syntax::Basic).contains("nothing to repeat"));
        assert_eq!(translate("a.*[", Syntax::Fixed).unwrap(), r"a\.\*\[");
    }

    fn matcher(patterns: &[&str], settings: Settings) -> Matcher {
        let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
        Matcher::new(&patterns, settings).unwrap()
    }

    const BASIC: Settings = Settings {
        syntax: Syntax::Basic,
        ignore_case: false,
        word: false,
        line: false,
        null_data: false,
    };

    #[test]
    fn matches_lines_words_and_cases() {
        let m = matcher(&["fo*", "bar"], BASIC);
        assert!(m.is_match(b"xbarx") && m.is_match(b"f") && !m.is_match(b"ba"));
        assert_eq!(m.matches(b"foo bar f"), [(0, 3), (4, 7), (8, 9)]);
        let m = matcher(
            &["ab"],
            Settings {
                ignore_case: true,
                ..BASIC
            },
        );
        assert!(m.is_match(b"xAbx"));
        let line = Settings {
            line: true,
            ..BASIC
        };
        assert!(matcher(&["a.c"], line).is_match(b"abc"));
        assert!(!matcher(&["a.c"], line).is_match(b"abcd"));
        let word = Settings {
            word: true,
            ..BASIC
        };
        let m = matcher(&["a"], word);
        assert!(m.is_match(b"x a y") && !m.is_match(b"ab"));
        assert_eq!(m.matches(b"a a,a ab"), [(0, 1), (2, 3), (4, 5)]);
        assert!(matcher(&[""], BASIC).is_match(b"anything"));
        assert!(!Matcher::new(&[], BASIC).unwrap().is_match(b"x"));
        assert!(Matcher::new(&["a{".into()], BASIC).is_ok());
        let bad = Matcher::new(&[r"\(".into()], BASIC).unwrap_err();
        assert!(bad.contains("unmatched"));
    }
}
