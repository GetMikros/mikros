//! Wildcard patterns, as the shell reads them: `*` for any characters,
//! `?` for one, `[...]` for one of a set, with ranges and `!` or `^`
//! to negate it, and `\` before a character that stands for itself.
//! `*` and `?` also match `/`.

/// One element of a pattern.
#[derive(Clone, Debug, PartialEq)]
enum Token {
    Char(char),
    /// `?`.
    One,
    /// `*`.
    Any,
    /// `[...]`: whether it is negated, and its ranges.
    Set(bool, Vec<(char, char)>),
}

/// A wildcard pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    tokens: Vec<Token>,
}

impl Pattern {
    pub fn new(text: &str) -> Pattern {
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
        Pattern { tokens }
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
            } else if tokens.get(p).is_some_and(|k| one(k, text[t])) {
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
}

/// Whether `token`, not `*`, matches `c`.
fn one(token: &Token, c: char) -> bool {
    match token {
        Token::Char(k) => *k == c,
        Token::One => true,
        Token::Any => false,
        Token::Set(negated, ranges) => {
            ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c)) != *negated
        }
    }
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
    let mut ranges = Vec::new();
    // One character of the set, escaped or not, and where it ends.
    let at = |i: usize| -> Option<(char, usize)> {
        match chars.get(i)? {
            '\\' => chars.get(i + 1).map(|&c| (c, i + 2)),
            &c => Some((c, i + 1)),
        }
    };
    loop {
        if chars.get(i) == Some(&']') && i > first {
            return Some((Token::Set(negated, ranges), i + 1));
        }
        let (lo, next) = at(i)?;
        match (chars.get(next), chars.get(next + 1)) {
            (Some('-'), Some(&hi)) if hi != ']' => {
                let (hi, after) = at(next + 1)?;
                ranges.push((lo, hi));
                i = after;
            }
            _ => {
                ranges.push((lo, lo));
                i = next;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, text: &str) -> bool {
        Pattern::new(pattern).matches(text)
    }

    #[test]
    fn matches_wildcards() {
        assert!(matches("*.o", "a.o"));
        assert!(matches("*.o", "dir/a.o"));
        assert!(!matches("*.o", "a.c"));
        assert!(matches("a?c", "abc") && matches("a?c", "a/c"));
        assert!(!matches("a?c", "ac"));
        assert!(matches("*", ""));
        assert!(matches("a*b*c", "aXbYbZc"));
        assert!(!matches("a*b*c", "aXbYbZ"));
        assert!(matches("plain", "plain") && !matches("plain", "plainer"));
    }

    #[test]
    fn matches_sets_and_escapes() {
        assert!(matches("[abc]x", "bx") && !matches("[abc]x", "dx"));
        assert!(matches("[a-c]", "b") && !matches("[a-c]", "d"));
        assert!(matches("[!a-c]", "d") && !matches("[^a-c]", "a"));
        assert!(matches("[]]", "]"));
        assert!(matches("\\*", "*") && !matches("\\*", "a"));
        assert!(matches("[", "[") && matches("a[b", "a[b"));
    }
}
