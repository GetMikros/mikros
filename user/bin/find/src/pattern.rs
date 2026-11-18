//! The patterns of find's `-name`, `-iname` and `-path`
//! (MIKSPEC-0098.R42), as POSIX's `fnmatch` reads them without flags.

/// One element of a pattern.
#[derive(Clone, Debug, PartialEq)]
enum Token {
    Char(char),
    /// `?`.
    One,
    /// `*`.
    Any,
    /// `[...]`: whether it is negated, and its ranges and classes.
    Set(bool, Vec<Member>),
}

#[derive(Clone, Debug, PartialEq)]
enum Member {
    Range(char, char),
    Class(String),
}

/// A pattern of `-name`, `-iname` and `-path`, as POSIX's `fnmatch`
/// reads it without flags: `*` and `?` also match `/` and a leading `.`.
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    tokens: Vec<Token>,
    fold: bool,
}

impl Pattern {
    /// The pattern of `text`, matching in any case when `fold`.
    pub fn new(text: &str, fold: bool) -> Pattern {
        let chars: Vec<char> = text.chars().collect();
        let mut tokens = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '*' => tokens.push(Token::Any),
                '?' => tokens.push(Token::One),
                '\\' if i + 1 < chars.len() => {
                    i += 1;
                    tokens.push(Token::Char(chars[i]));
                }
                '[' => match set(&chars, i + 1) {
                    Some((token, next)) => {
                        tokens.push(token);
                        i = next;
                        continue;
                    }
                    None => tokens.push(Token::Char('[')),
                },
                c => tokens.push(Token::Char(c)),
            }
            i += 1;
        }
        Pattern { tokens, fold }
    }

    /// Whether the pattern matches all of `text`.
    pub fn matches(&self, text: &str) -> bool {
        let text: Vec<char> = text.chars().collect();
        let tokens = &self.tokens;
        let (mut p, mut t) = (0, 0);
        // Where the last `*` is, and where in the text it stops for now.
        let mut star: Option<(usize, usize)> = None;
        while t < text.len() {
            if tokens.get(p) == Some(&Token::Any) {
                star = Some((p, t));
                p += 1;
            } else if tokens.get(p).is_some_and(|k| self.one(k, text[t])) {
                p += 1;
                t += 1;
            } else if let Some((at, from)) = star {
                p = at + 1;
                t = from + 1;
                star = Some((at, from + 1));
            } else {
                return false;
            }
        }
        tokens[p..].iter().all(|k| *k == Token::Any)
    }

    /// Whether `token`, not `*`, matches `c`.
    fn one(&self, token: &Token, c: char) -> bool {
        let same = |a: char, b: char| a == b || (self.fold && lower(a) == lower(b));
        match token {
            Token::Char(k) => same(*k, c),
            Token::One => true,
            Token::Any => false,
            Token::Set(negated, members) => {
                let forms = match self.fold {
                    true => vec![c, lower(c), upper(c)],
                    false => vec![c],
                };
                let inside = members.iter().any(|m| {
                    forms.iter().any(|&c| match m {
                        Member::Range(lo, hi) => (*lo..=*hi).contains(&c),
                        Member::Class(name) => class(name, c),
                    })
                });
                inside != *negated
            }
        }
    }
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn upper(c: char) -> char {
    c.to_uppercase().next().unwrap_or(c)
}

/// The set that starts at `start`, after its `[`, and where the pattern
/// goes on after its `]`; none when it has no `]`.
fn set(chars: &[char], start: usize) -> Option<(Token, usize)> {
    let mut i = start;
    let negated = matches!(chars.get(i), Some('!' | '^'));
    if negated {
        i += 1;
    }
    let first = i;
    let mut members = Vec::new();
    // One character of the set, escaped or not, and where it ends.
    let at = |i: usize| -> Option<(char, usize)> {
        match chars.get(i)? {
            '\\' => chars.get(i + 1).map(|&c| (c, i + 2)),
            &c => Some((c, i + 1)),
        }
    };
    loop {
        let c = *chars.get(i)?;
        if c == ']' && i > first {
            return Some((Token::Set(negated, members), i + 1));
        }
        if c == '['
            && chars.get(i + 1) == Some(&':')
            && let Some(end) = (i + 2..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == ':' && chars[j + 1] == ']')
        {
            members.push(Member::Class(chars[i + 2..end].iter().collect()));
            i = end + 2;
            continue;
        }
        let (lo, next) = at(i)?;
        match (chars.get(next), chars.get(next + 1)) {
            (Some('-'), Some(&hi)) if hi != ']' => {
                let (hi, after) = at(next + 1)?;
                members.push(Member::Range(lo, hi));
                i = after;
            }
            _ => {
                members.push(Member::Range(lo, lo));
                i = next;
            }
        }
    }
}

/// Whether `c` is in the class `[:name:]`.
fn class(name: &str, c: char) -> bool {
    match name {
        "alnum" => c.is_alphanumeric(),
        "alpha" => c.is_alphabetic(),
        "blank" => c == ' ' || c == '\t',
        "cntrl" => c.is_control(),
        "digit" => c.is_ascii_digit(),
        "graph" => !c.is_control() && !c.is_whitespace(),
        "lower" => c.is_lowercase(),
        "print" => !c.is_control(),
        "punct" => c.is_ascii_punctuation(),
        "space" => c.is_whitespace(),
        "upper" => c.is_uppercase(),
        "xdigit" => c.is_ascii_hexdigit(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_patterns_as_fnmatch_does() {
        let m = |p: &str, t: &str| Pattern::new(p, false).matches(t);
        assert!(m("*.txt", "a.txt") && m("*.txt", ".txt") && !m("*.txt", "a.txt~"));
        assert!(m("a?c", "abc") && !m("a?c", "ac"));
        assert!(m("*", "") && m("**a*", "xa") && m("*a*b", "aab"));
        assert!(m("[abc]x", "bx") && !m("[!abc]x", "bx") && m("[^a]x", "bx"));
        assert!(m("[a-c]", "b") && !m("[a-c]", "d") && m("[]]", "]") && m("[!]]", "a"));
        assert!(m("[[:digit:]]*", "7z") && !m("[[:upper:]]", "a"));
        assert!(m("\\*", "*") && !m("\\*", "a") && m("[", "[") && m("a[b", "a[b"));
        assert!(m("./a/*", "./a/b/c"));
        let i = |p: &str, t: &str| Pattern::new(p, true).matches(t);
        assert!(i("*.TXT", "a.txt") && i("[A-C]", "b") && !i("x", "y"));
    }
}
