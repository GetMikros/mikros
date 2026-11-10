//! Expressions: operators by precedence, operands and their field and
//! item access, lists, records, blocks and parentheses.

use super::{Parser, Result};
use crate::ast::*;

/// The binary operators of each precedence level, from the lowest.
const LEVELS: [&[(&str, Binary)]; 6] = [
    &[("or", Binary::Or)],
    &[("and", Binary::And)],
    &[
        ("==", Binary::Equal),
        ("!=", Binary::NotEqual),
        ("<=", Binary::LessEqual),
        (">=", Binary::GreaterEqual),
        ("=~", Binary::Matches),
        ("!~", Binary::NotMatches),
        ("<", Binary::Less),
        (">", Binary::Greater),
    ],
    &[("??", Binary::Default)],
    &[("+", Binary::Add), ("-", Binary::Subtract)],
    &[
        ("*", Binary::Multiply),
        ("/", Binary::Divide),
        ("%", Binary::Remainder),
    ],
];

impl Parser<'_> {
    /// Whether an expression starts here, as a condition's first token
    /// decides.
    pub(super) fn at_expression(&self) -> bool {
        match self.peek() {
            Some('$' | '(' | '"' | '\'' | '[' | '{') => true,
            _ if self.at_number() => true,
            _ => ["not", "true", "false", "null"]
                .iter()
                .any(|k| self.at_keyword(k)),
        }
    }

    /// An expression.
    pub(super) fn expression(&mut self) -> Result<Expr> {
        self.level(0)
    }

    fn level(&mut self, level: usize) -> Result<Expr> {
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.level(level + 1)?;
        loop {
            let save = self.pos;
            self.skip_spaces();
            let Some(&(text, op)) = LEVELS[level]
                .iter()
                .find(|(text, _)| self.at_operator(text))
            else {
                self.pos = save;
                return Ok(left);
            };
            self.pos += text.len();
            // An operator at the end of a line continues the expression.
            self.skip_blank();
            if self.peek().is_none() {
                return Err(self.incomplete(&format!("an operand is missing after `{text}`")));
            }
            let right = self.level(level + 1)?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
    }

    /// Whether the operator `text` is here.
    fn at_operator(&self, text: &str) -> bool {
        if text.chars().all(|c| c.is_ascii_alphabetic()) {
            return self.at_keyword(text);
        }
        if !self.looking_at(text) {
            return false;
        }
        let next = self.peek_at(text.len());
        match text {
            // Not `<=`, `>=`, `=~`, `==`.
            "<" | ">" => next != Some('='),
            // `-` before a digit right after a space is a negative operand,
            // which needs an operator before it.
            _ => true,
        }
    }

    fn unary(&mut self) -> Result<Expr> {
        self.skip_spaces();
        if self.peek() == Some('-') && !self.looking_at("--") {
            self.pos += 1;
            let operand = self.unary()?;
            return Ok(Expr::Unary(Unary::Negate, Box::new(operand)));
        }
        if self.at_keyword("not") {
            self.pos += 3;
            let operand = self.unary()?;
            return Ok(Expr::Unary(Unary::Not, Box::new(operand)));
        }
        self.operand()
    }

    /// An operand with the `.field` and `.0` accesses that follow it.
    pub(super) fn operand(&mut self) -> Result<Expr> {
        self.skip_spaces();
        let start = self.pos;
        let mut expr = match self.peek() {
            None => return Err(self.incomplete("an expression is missing")),
            Some('$') => Expr::Variable(self.variable()?),
            Some('"') => Expr::String(self.double_quoted()?),
            Some('\'') => Expr::Literal(Literal::String(self.single_quoted()?)),
            Some('(') => self.paren()?,
            Some('[') => self.list()?,
            Some('{') => self.record_or_block()?,
            _ if self.at_number() => Expr::Literal(self.number()?),
            _ if self.at_keyword("true") => {
                self.pos += 4;
                Expr::Literal(Literal::Bool(true))
            }
            _ if self.at_keyword("false") => {
                self.pos += 5;
                Expr::Literal(Literal::Bool(false))
            }
            _ if self.at_keyword("null") => {
                self.pos += 4;
                Expr::Literal(Literal::Null)
            }
            _ => {
                return Err(self.error_at(
                    start,
                    "an expression is expected here; quote text in expressions",
                ));
            }
        };
        // Variables took their accesses already.
        while self.peek() == Some('.') && !self.looking_at("..") {
            match self.peek_at(1) {
                Some(c) if c.is_ascii_digit() => {
                    self.pos += 1;
                    let digits = self.take_while(|c| c.is_ascii_digit());
                    let index = digits
                        .parse()
                        .map_err(|_| self.error("the index is too large"))?;
                    expr = Expr::Access(Box::new(expr), Access::Item(index));
                }
                Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                    self.pos += 1;
                    let field = self.take_while(|c| c.is_ascii_alphanumeric() || c == '_');
                    expr = Expr::Access(Box::new(expr), Access::Field(field));
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// `(expr)` or `(command)`: an expression when an expression starts
    /// it, as a condition's first token decides, and otherwise a
    /// pipeline.
    pub(super) fn paren(&mut self) -> Result<Expr> {
        self.expect('(', "`(`")?;
        self.skip_blank();
        let expr = if self.at_expression() || self.peek() == Some('-') && self.at_negative() {
            self.expression()?
        } else {
            Expr::Pipeline(Box::new(self.pipeline(false)?))
        };
        self.skip_blank();
        self.expect(')', "`)`")?;
        // `(command)?` returns its error to the caller.
        if self.peek() == Some('?') {
            self.pos += 1;
            return Ok(Expr::Propagate(Box::new(expr)));
        }
        Ok(expr)
    }

    /// Whether a `-` here negates a number rather than starting an option.
    fn at_negative(&self) -> bool {
        self.peek_at(1)
            .is_some_and(|c| c.is_ascii_digit() || c == '$' || c == '(')
    }

    /// `[a b c]`: its items are expressions, and bare words in it are
    /// words, strings once expanded.
    fn list(&mut self) -> Result<Expr> {
        self.expect('[', "`[`")?;
        let mut items = Vec::new();
        loop {
            self.skip_blank();
            match self.peek() {
                None => return Err(self.incomplete("`]` is missing")),
                Some(']') => {
                    self.pos += 1;
                    return Ok(Expr::List(items));
                }
                Some(',') => self.pos += 1,
                _ if self.at_expression() => items.push(self.operand()?),
                _ => items.push(Expr::Words(vec![self.word()?])),
            }
        }
    }

    /// `{name: value, …}`, or a block when it does not start like a record.
    fn record_or_block(&mut self) -> Result<Expr> {
        let start = self.pos;
        self.pos += 1;
        self.skip_blank();
        let record = match self.peek() {
            Some('}') => true,
            Some('"' | '\'') => {
                let save = self.pos;
                let key = self.key();
                self.skip_spaces();
                let colon = key.is_ok() && self.peek() == Some(':');
                self.pos = save;
                colon
            }
            _ => self.peek_name().is_some_and(|name| {
                let after = self.pos + name.chars().count();
                self.chars.get(after) == Some(&':')
            }),
        };
        if !record {
            self.pos = start + 1;
            return Ok(Expr::Block(self.statements(Some('}'))?));
        }
        let mut fields = Vec::new();
        loop {
            self.skip_blank();
            match self.peek() {
                None => return Err(self.incomplete("`}` is missing")),
                Some('}') => {
                    self.pos += 1;
                    return Ok(Expr::Record(fields));
                }
                Some(',') => self.pos += 1,
                _ => {
                    let key = self.key()?;
                    self.skip_spaces();
                    self.expect(':', "`:`")?;
                    self.skip_spaces();
                    fields.push((key, self.expression()?));
                }
            }
        }
    }
}
