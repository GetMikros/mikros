//! Parsing msh's command language into its syntax tree.

mod control;
mod expr;
mod literal;
mod words;

pub use literal::days_from_civil;

use crate::ast::*;
use std::fmt;

/// Why a script cannot be parsed, and the line where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub message: String,
    /// The text ended inside an open bracket, quote or statement, so that
    /// an interactive shell can read another line.
    pub incomplete: bool,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

type Result<T> = std::result::Result<T, Error>;

/// The keywords that start a statement.
const KEYWORDS: [&str; 10] = [
    "let", "if", "for", "while", "match", "fn", "param", "try", "use", "return",
];

/// Parses `source` into the statements of a block.
pub fn parse(source: &str) -> Result<Block> {
    let chars: Vec<char> = source.chars().collect();
    let mut parser = Parser {
        chars: &chars,
        pos: 0,
    };
    let block = parser.statements(None)?;
    Ok(block)
}

pub(crate) struct Parser<'a> {
    chars: &'a [char],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).copied()
    }

    fn looking_at(&self, text: &str) -> bool {
        text.chars()
            .enumerate()
            .all(|(i, c)| self.peek_at(i) == Some(c))
    }

    fn take_while(&mut self, f: impl Fn(char) -> bool) -> String {
        let start = self.pos;
        while self.peek().is_some_and(&f) {
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }

    /// The line of position `at`, from 1.
    fn line_of(&self, at: usize) -> usize {
        1 + self.chars[..at.min(self.chars.len())]
            .iter()
            .filter(|&&c| c == '\n')
            .count()
    }

    fn error_at(&self, at: usize, message: &str) -> Error {
        Error {
            line: self.line_of(at),
            message: message.into(),
            incomplete: false,
        }
    }

    fn error(&self, message: &str) -> Error {
        self.error_at(self.pos, message)
    }

    /// An error for text that ended too early.
    fn incomplete(&self, message: &str) -> Error {
        Error {
            incomplete: true,
            ..self.error(message)
        }
    }

    /// Skips spaces, tabs and `\` line continuations.
    fn skip_spaces(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t') => self.pos += 1,
                Some('\\') if self.peek_at(1) == Some('\n') => self.pos += 2,
                Some('\\') if self.peek_at(1) == Some('\r') && self.peek_at(2) == Some('\n') => {
                    self.pos += 3
                }
                _ => return,
            }
        }
    }

    /// Skips a comment, from `#` to the end of the line.
    fn skip_comment(&mut self) {
        if self.peek() == Some('#') {
            while self.peek().is_some_and(|c| c != '\n') {
                self.pos += 1;
            }
        }
    }

    /// Skips spaces, comments, newlines and `;`.
    fn skip_separators(&mut self) {
        loop {
            self.skip_spaces();
            self.skip_comment();
            match self.peek() {
                Some('\n' | '\r' | ';') => self.pos += 1,
                _ => return,
            }
        }
    }

    /// Skips spaces, comments and newlines, as inside brackets.
    fn skip_blank(&mut self) {
        loop {
            self.skip_spaces();
            self.skip_comment();
            match self.peek() {
                Some('\n' | '\r') => self.pos += 1,
                _ => return,
            }
        }
    }

    /// The identifier that starts here, without taking it.
    fn peek_name(&self) -> Option<String> {
        let first = self.peek()?;
        if !(first.is_ascii_alphabetic() || first == '_') {
            return None;
        }
        let mut name = String::new();
        let mut i = 0;
        while let Some(c) = self.peek_at(i) {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                name.push(c);
                i += 1;
            } else {
                break;
            }
        }
        Some(name)
    }

    /// An identifier.
    fn name(&mut self) -> Result<String> {
        let name = self
            .peek_name()
            .ok_or_else(|| self.error("a name is expected here"))?;
        self.pos += name.chars().count();
        Ok(name)
    }

    /// Whether the keyword `word` is here, as a whole word.
    fn at_keyword(&self, word: &str) -> bool {
        self.looking_at(word)
            && !self
                .peek_at(word.chars().count())
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    }

    fn expect(&mut self, c: char, what: &str) -> Result<()> {
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else if self.peek().is_none() {
            Err(self.incomplete(&format!("{what} is missing")))
        } else {
            Err(self.error(&format!("{what} is expected here")))
        }
    }

    /// Statements up to `end`, `}` for a block or `None` for the end of
    /// the text, which is taken.
    fn statements(&mut self, end: Option<char>) -> Result<Block> {
        let mut statements = Vec::new();
        loop {
            self.skip_separators();
            match (self.peek(), end) {
                (None, None) => break,
                (None, Some(_)) => return Err(self.incomplete("`}` is missing")),
                (Some(c), Some(e)) if c == e => {
                    self.pos += 1;
                    break;
                }
                (Some('}'), None) => return Err(self.error("this `}` closes nothing")),
                _ => {}
            }
            statements.push(self.statement()?);
            self.skip_spaces();
            self.skip_comment();
            match self.peek() {
                None | Some('\n' | '\r' | ';') => {}
                Some('}') if end == Some('}') => {}
                _ => return Err(self.error("a statement ends here, before a newline or `;`")),
            }
        }
        Ok(Block { statements })
    }

    /// A block in braces.
    fn block(&mut self) -> Result<Block> {
        self.skip_spaces();
        self.expect('{', "`{`")?;
        self.statements(Some('}'))
    }

    fn statement(&mut self) -> Result<Statement> {
        let line = self.line_of(self.pos);
        let kind = self.statement_kind()?;
        Ok(Statement { line, kind })
    }

    fn statement_kind(&mut self) -> Result<StatementKind> {
        for keyword in KEYWORDS {
            if self.at_keyword(keyword) {
                self.pos += keyword.len();
                self.skip_spaces();
                return self.keyword(keyword);
            }
        }
        if let Some(assign) = self.assignment()? {
            return Ok(assign);
        }
        if self.peek() == Some('(') {
            let expr = self.paren()?;
            self.skip_spaces();
            if self.peek() != Some('|') {
                return Ok(StatementKind::Expr(expr));
            }
            let mut pipeline = Pipeline {
                commands: vec![Command::Expr(expr)],
                background: false,
            };
            self.rest_of_pipeline(&mut pipeline, false)?;
            return Ok(StatementKind::Pipeline(pipeline));
        }
        Ok(StatementKind::Pipeline(self.pipeline(false)?))
    }

    /// `name = value`, `name += value` or `env.NAME = value`, if one
    /// starts here.
    fn assignment(&mut self) -> Result<Option<StatementKind>> {
        let Some(name) = self.peek_name() else {
            return Ok(None);
        };
        let start = self.pos;
        self.pos += name.chars().count();
        let target = if name == "env" && self.peek() == Some('.') {
            self.pos += 1;
            match self.peek_name() {
                Some(var) => {
                    self.pos += var.chars().count();
                    Target::Environment(var)
                }
                None => {
                    self.pos = start;
                    return Ok(None);
                }
            }
        } else {
            Target::Variable(name)
        };
        let tight = self.peek() == Some('=') && self.peek_at(1).is_some_and(|c| !c.is_whitespace());
        if tight && self.command_follows() {
            // `NAME=value command`, which sets a variable for a command.
            self.pos = start;
            return Ok(None);
        }
        self.skip_spaces();
        let append = self.looking_at("+=");
        let assigns = append || (self.peek() == Some('=') && self.peek_at(1) != Some('='));
        if !assigns {
            self.pos = start;
            return Ok(None);
        }
        self.pos += if append { 2 } else { 1 };
        self.skip_spaces();
        let value = self.expression()?;
        Ok(Some(StatementKind::Assign {
            target,
            append,
            value,
        }))
    }

    /// Whether, after the `=value` word here, the statement goes on with
    /// a command.
    fn command_follows(&mut self) -> bool {
        let save = self.pos;
        self.pos += 1;
        let words = self.word().is_ok();
        self.skip_spaces();
        let follows = words && !matches!(self.peek(), None | Some('\n' | '\r' | ';' | '}' | '#'));
        self.pos = save;
        follows
    }

    /// A record's key: a name or a quoted string.
    fn key(&mut self) -> Result<String> {
        match self.peek() {
            Some('"' | '\'') => match self.operand()? {
                Expr::String(parts) => match parts.as_slice() {
                    [] => Ok(String::new()),
                    [StringPart::Text(text)] => Ok(text.clone()),
                    _ => Err(self.error("a key cannot interpolate")),
                },
                Expr::Literal(Literal::String(text)) => Ok(text),
                _ => Err(self.error("a key is expected here")),
            },
            _ => self.name(),
        }
    }
}

#[cfg(test)]
mod tests;
