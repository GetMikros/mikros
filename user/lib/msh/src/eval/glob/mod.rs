//! Glob patterns: `*` and `?`, which do not match `/`, bracket
//! expressions `[abc]`, `[a-z]` and `[!abc]`, and `**` as a whole path
//! component, which matches any number of folders. `*` and `?` do not
//! match a name's leading `.` unless the pattern's component starts
//! with one.

/// A part of a glob pattern: text that must match literally, as from
/// quotes and variables, or text whose wildcards count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Literal(String),
    Pattern(String),
}

/// A character of a pattern, after quoting was resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Char(char),
    Any,
    One,
    Class {
        negated: bool,
        items: Vec<(char, char)>,
    },
}

/// Whether `text`'s wildcards make it a pattern.
pub fn has_wildcards(text: &str) -> bool {
    text.contains(['*', '?']) || (text.contains('[') && text.contains(']'))
}

/// The tokens of `pieces`, as path components.
fn components(pieces: &[Piece]) -> Vec<Vec<Token>> {
    let mut components = vec![Vec::new()];
    for piece in pieces {
        match piece {
            Piece::Literal(text) => {
                for c in text.chars() {
                    if c == '/' {
                        components.push(Vec::new());
                    } else {
                        components.last_mut().expect("one").push(Token::Char(c));
                    }
                }
            }
            Piece::Pattern(text) => {
                let chars: Vec<char> = text.chars().collect();
                let mut i = 0;
                while i < chars.len() {
                    let c = chars[i];
                    let token = match c {
                        '/' => {
                            components.push(Vec::new());
                            i += 1;
                            continue;
                        }
                        '*' => Token::Any,
                        '?' => Token::One,
                        '[' => match class(&chars[i + 1..]) {
                            Some((token, used)) => {
                                i += used + 1;
                                components.last_mut().expect("one").push(token);
                                continue;
                            }
                            None => Token::Char('['),
                        },
                        c => Token::Char(c),
                    };
                    components.last_mut().expect("one").push(token);
                    i += 1;
                }
            }
        }
    }
    components
}

/// A bracket expression after its `[`, and how many characters it took
/// with its `]`.
fn class(chars: &[char]) -> Option<(Token, usize)> {
    let mut i = 0;
    let negated = chars.first() == Some(&'!');
    if negated {
        i += 1;
    }
    let mut items = Vec::new();
    let mut first = true;
    while i < chars.len() {
        let c = chars[i];
        if c == ']' && !first {
            return Some((Token::Class { negated, items }, i + 1));
        }
        first = false;
        if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|&e| e != ']') {
            items.push((c, chars[i + 2]));
            i += 3;
        } else {
            items.push((c, c));
            i += 1;
        }
    }
    None
}

/// Whether `name` matches the component `tokens`.
fn matches(tokens: &[Token], name: &str) -> bool {
    let name: Vec<char> = name.chars().collect();
    let dot_literal = matches!(tokens.first(), Some(Token::Char('.')));
    if name.first() == Some(&'.') && !dot_literal {
        return false;
    }
    match_at(tokens, &name)
}

fn match_at(tokens: &[Token], name: &[char]) -> bool {
    match tokens.split_first() {
        None => name.is_empty(),
        Some((Token::Any, rest)) => (0..=name.len()).any(|skip| match_at(rest, &name[skip..])),
        Some((token, rest)) => match name.split_first() {
            None => false,
            Some((&c, name_rest)) => {
                let ok = match token {
                    Token::Char(t) => *t == c,
                    Token::One => true,
                    Token::Class { negated, items } => {
                        items.iter().any(|&(lo, hi)| lo <= c && c <= hi) != *negated
                    }
                    Token::Any => unreachable!("handled above"),
                };
                ok && match_at(rest, name_rest)
            }
        },
    }
}

/// Whether a component has wildcards.
fn literal(tokens: &[Token]) -> Option<String> {
    tokens
        .iter()
        .map(|t| match t {
            Token::Char(c) => Some(*c),
            _ => None,
        })
        .collect()
}

/// The entries of a folder: their names, and whether each is a folder.
pub trait Folders {
    fn entries(&self, path: &str) -> Option<Vec<(String, bool)>>;
}

/// The paths that `pieces` match, sorted, relative as the pattern is,
/// below the current folder for a relative one. Matching nothing is an
/// error.
pub fn expand(pieces: &[Piece], folders: &dyn Folders) -> Result<Vec<String>, String> {
    let components = components(pieces);
    let absolute = components.first().is_some_and(Vec::is_empty) && components.len() > 1;
    let (start, components) = match absolute {
        true => ("/".to_owned(), &components[1..]),
        false => (String::new(), &components[..]),
    };
    let mut found = Vec::new();
    walk(&start, components, folders, &mut found);
    found.sort();
    found.dedup();
    if found.is_empty() {
        let text: String = pieces
            .iter()
            .map(|p| match p {
                Piece::Literal(t) | Piece::Pattern(t) => t.as_str(),
            })
            .collect();
        return Err(format!("no file matches `{text}`"));
    }
    Ok(found)
}

/// `prefix` joined with `name`.
fn join(prefix: &str, name: &str) -> String {
    match prefix {
        "" => name.to_owned(),
        p if p.ends_with('/') => format!("{p}{name}"),
        p => format!("{p}/{name}"),
    }
}

fn walk(prefix: &str, components: &[Vec<Token>], folders: &dyn Folders, found: &mut Vec<String>) {
    let Some((first, rest)) = components.split_first() else {
        found.push(prefix.to_owned());
        return;
    };
    let folder = if prefix.is_empty() { "." } else { prefix };
    if matches!(first.as_slice(), [Token::Any, Token::Any]) {
        // `**`: this folder and every folder below it, not hidden ones.
        walk(prefix, rest, folders, found);
        for (name, is_folder) in folders.entries(folder).unwrap_or_default() {
            if is_folder && !name.starts_with('.') {
                walk(&join(prefix, &name), components, folders, found);
            }
        }
        return;
    }
    if let Some(name) = literal(first) {
        // A component without wildcards must exist, but is not listed.
        let path = join(prefix, &name);
        if rest.is_empty() {
            let parent = folders.entries(folder).unwrap_or_default();
            if parent.iter().any(|(n, _)| *n == name) {
                found.push(path);
            }
        } else {
            walk(&path, rest, folders, found);
        }
        return;
    }
    for (name, is_folder) in folders.entries(folder).unwrap_or_default() {
        if matches(first, &name) && (rest.is_empty() || is_folder) {
            walk(&join(prefix, &name), rest, folders, found);
        }
    }
}

#[cfg(test)]
mod tests;
