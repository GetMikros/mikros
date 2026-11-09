//! Splits MIDL source text into tokens.

use crate::{Error, Span};

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Ident(String),
    /// An unsigned integer literal, decimal or hexadecimal.
    Int(u64),
    Str(String),
    /// The text of a `///` comment line, without the slashes.
    Doc(String),
    /// `->`.
    Arrow,
    /// Any other punctuation character.
    Punct(char),
}

/// The tokens of `text`, each with where it starts, or the first lexical
/// error. `//` comments are dropped; `///` comments become `Doc` tokens.
pub fn tokens(file: &str, text: &str) -> Result<Vec<(Token, Span)>, Error> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let (mut i, mut line, mut col) = (0, 1, 1);
    let span = |line, col| Span {
        file: file.to_string(),
        line,
        col,
    };
    while i < chars.len() {
        let c = chars[i];
        let start = span(line, col);
        let mut advance = |n: usize, i: &mut usize| {
            for _ in 0..n {
                if chars[*i] == '\n' {
                    line += 1;
                    col = 1;
                } else {
                    col += 1;
                }
                *i += 1;
            }
        };
        if c.is_whitespace() {
            advance(1, &mut i);
        } else if text_at(&chars, i, "//") {
            let end = (i..chars.len())
                .find(|&j| chars[j] == '\n')
                .unwrap_or(chars.len());
            if text_at(&chars, i, "///") {
                let doc: String = chars[i + 3..end].iter().collect();
                let doc = doc.strip_prefix(' ').map(str::to_string).unwrap_or(doc);
                out.push((Token::Doc(doc), start));
            }
            advance(end - i, &mut i);
        } else if c.is_ascii_alphabetic() || c == '_' {
            let end = (i..chars.len())
                .find(|&j| !(chars[j].is_ascii_alphanumeric() || chars[j] == '_'))
                .unwrap_or(chars.len());
            out.push((Token::Ident(chars[i..end].iter().collect()), start));
            advance(end - i, &mut i);
        } else if c.is_ascii_digit() {
            let end = (i..chars.len())
                .find(|&j| !(chars[j].is_ascii_alphanumeric() || chars[j] == '_'))
                .unwrap_or(chars.len());
            let literal: String = chars[i..end].iter().filter(|&&c| c != '_').collect();
            let value = match literal.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16),
                None => literal.parse(),
            };
            let value = value.map_err(|_| start.error("invalid integer literal"))?;
            out.push((Token::Int(value), start));
            advance(end - i, &mut i);
        } else if c == '"' {
            let mut j = i + 1;
            let mut value = String::new();
            loop {
                match chars.get(j) {
                    None | Some('\n') => return Err(start.error("unterminated string literal")),
                    Some('"') => break,
                    Some('\\') => {
                        value.push(match chars.get(j + 1) {
                            Some('n') => '\n',
                            Some('"') => '"',
                            Some('\\') => '\\',
                            _ => return Err(start.error("invalid escape in string literal")),
                        });
                        j += 2;
                    }
                    Some(&c) => {
                        value.push(c);
                        j += 1;
                    }
                }
            }
            out.push((Token::Str(value), start));
            advance(j + 1 - i, &mut i);
        } else if text_at(&chars, i, "->") {
            out.push((Token::Arrow, start));
            advance(2, &mut i);
        } else if "{}()<>,;:=?|@.-".contains(c) {
            out.push((Token::Punct(c), start));
            advance(1, &mut i);
        } else {
            return Err(start.error(format!("unexpected character {c:?}")));
        }
    }
    Ok(out)
}

fn text_at(chars: &[char], i: usize, text: &str) -> bool {
    text.chars()
        .enumerate()
        .all(|(k, c)| chars.get(i + k) == Some(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<Token> {
        tokens("t.midl", text)
            .unwrap()
            .into_iter()
            .map(|(t, _)| t)
            .collect()
    }

    #[test]
    fn splits_tokens_and_keeps_doc_comments() {
        use Token::*;
        assert_eq!(
            kinds("/// Doc.\n// skipped\nconst A u32 = 0x1_0; @1 F() -> ();"),
            [
                Doc("Doc.".into()),
                Ident("const".into()),
                Ident("A".into()),
                Ident("u32".into()),
                Punct('='),
                Int(16),
                Punct(';'),
                Punct('@'),
                Int(1),
                Ident("F".into()),
                Punct('('),
                Punct(')'),
                Arrow,
                Punct('('),
                Punct(')'),
                Punct(';'),
            ]
        );
    }

    #[test]
    fn reports_positions() {
        let err = tokens("t.midl", "a\n  #").unwrap_err();
        assert_eq!((err.span.line, err.span.col), (2, 3));
        assert!(tokens("t.midl", "\"open").is_err());
    }
}
