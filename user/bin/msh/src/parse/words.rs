//! Commands, pipelines, words, quoted strings and redirections.

use super::{Error, Parser, Result};
use crate::ast::*;

/// Whether `c` ends a bare word.
fn special(c: char) -> bool {
    c.is_whitespace() || "|;&><(){}[]".contains(c)
}

impl Parser<'_> {
    /// A pipeline, up to the end of its statement, or up to `{` when
    /// `before_block`, as a condition is.
    pub(super) fn pipeline(&mut self, before_block: bool) -> Result<Pipeline> {
        let mut pipeline = Pipeline {
            commands: vec![self.command(before_block)?],
            background: false,
        };
        self.rest_of_pipeline(&mut pipeline, before_block)?;
        Ok(pipeline)
    }

    /// The commands after a pipeline's first, and its `&`.
    pub(super) fn rest_of_pipeline(
        &mut self,
        pipeline: &mut Pipeline,
        before_block: bool,
    ) -> Result<()> {
        loop {
            self.skip_spaces();
            match self.peek() {
                Some('|') if self.peek_at(1) != Some('|') => {
                    self.pos += 1;
                    self.skip_blank();
                    if self.peek().is_none() {
                        return Err(self.incomplete("a command is missing after `|`"));
                    }
                    pipeline.commands.push(self.command(before_block)?);
                }
                Some('&') if self.peek_at(1) != Some('&') => {
                    self.pos += 1;
                    pipeline.background = true;
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }
    }

    /// A command: an expression in parentheses, or a simple command.
    fn command(&mut self, before_block: bool) -> Result<Command> {
        self.skip_spaces();
        if self.peek() == Some('(') {
            return Ok(Command::Expr(self.operand()?));
        }
        let mut simple = Simple {
            environment: Vec::new(),
            words: Vec::new(),
            redirections: Vec::new(),
        };
        // NAME=value and env.NAME=value words before the command.
        while let Some((name, after)) = self.environment_word() {
            self.pos = after + 1;
            let value = match self.peek() {
                Some(c) if !special(c) => self.word()?,
                _ => Word { parts: Vec::new() },
            };
            simple.environment.push((name, value));
            self.skip_spaces();
        }
        loop {
            self.skip_spaces();
            match self.peek() {
                None | Some('\n' | '\r' | ';' | '|' | '&' | ')' | '}') => break,
                Some('{') if before_block => break,
                Some('{') => {
                    self.pos += 1;
                    simple
                        .words
                        .push(Argument::Block(self.statements(Some('}'))?));
                }
                Some('[') => {
                    let list = self.operand()?;
                    simple.words.push(Argument::Word(Word {
                        parts: vec![WordPart::Expr(list)],
                    }));
                }
                Some('(') => {
                    let expr = self.operand()?;
                    simple.words.push(Argument::Word(Word {
                        parts: vec![WordPart::Expr(expr)],
                    }));
                }
                Some(']') => return Err(self.error("this `]` closes nothing")),
                _ => {
                    if let Some(redirection) = self.redirection()? {
                        simple.redirections.push(redirection);
                        continue;
                    }
                    let word = self.word()?;
                    simple.words.push(Argument::Word(word));
                }
            }
        }
        if simple.words.is_empty() {
            if simple.environment.is_empty() && simple.redirections.is_empty() {
                return Err(match self.peek() {
                    None => self.incomplete("a command is missing"),
                    _ => self.error("a command is expected here"),
                });
            }
            return Err(self.error("a command is missing after these words"));
        }
        Ok(Command::Simple(simple))
    }

    /// The name of the `NAME=value` or `env.NAME=value` word that starts
    /// here, if one does, and where its `=` is.
    fn environment_word(&self) -> Option<(String, usize)> {
        let mut name = self.peek_name()?;
        let mut after = self.pos + name.chars().count();
        if name == "env" && self.chars.get(after) == Some(&'.') {
            let rest = Parser {
                pos: after + 1,
                ..*self
            };
            name = rest.peek_name()?;
            after = rest.pos + name.chars().count();
        }
        (self.chars.get(after) == Some(&'=') && !name.contains('-')).then_some((name, after))
    }

    /// A redirection, if one starts here.
    fn redirection(&mut self) -> Result<Option<Redirection>> {
        let (kind, length, target) = if self.looking_at("2>&1") {
            (RedirectionKind::ErrorToOutput, 4, false)
        } else if self.looking_at(">&2") {
            (RedirectionKind::OutputToError, 3, false)
        } else if self.looking_at("2>") {
            (RedirectionKind::Error, 2, true)
        } else if self.looking_at(">>") {
            (RedirectionKind::Append, 2, true)
        } else if self.looking_at(">") {
            (RedirectionKind::Output, 1, true)
        } else if self.looking_at("<") {
            (RedirectionKind::Input, 1, true)
        } else {
            return Ok(None);
        };
        self.pos += length;
        if !target {
            return Ok(Some(Redirection {
                kind,
                target: Word { parts: Vec::new() },
            }));
        }
        self.skip_spaces();
        match self.peek() {
            Some(c) if !special(c) || c == '(' => {}
            None => return Err(self.incomplete("a redirection's file is missing")),
            _ => return Err(self.error("a redirection's file is expected here")),
        }
        let target = if self.peek() == Some('(') {
            Word {
                parts: vec![WordPart::Expr(self.paren()?)],
            }
        } else {
            self.word()?
        };
        Ok(Some(Redirection { kind, target }))
    }

    /// A word: bare text, quoted strings and variables up to whitespace or
    /// a special character.
    pub(super) fn word(&mut self) -> Result<Word> {
        let start = self.pos;
        let mut parts: Vec<WordPart> = Vec::new();
        let mut bare = String::new();
        let flush = |bare: &mut String, parts: &mut Vec<WordPart>| {
            if !bare.is_empty() {
                parts.push(WordPart::Bare(std::mem::take(bare)));
            }
        };
        while let Some(c) = self.peek() {
            if c == '[' && !(parts.is_empty() && bare.is_empty()) {
                // A `[` right after the word's text belongs to it, up to its
                // matching `]`, as a glob's bracket expression.
                bare.push_str(&self.bracket()?);
                continue;
            }
            if special(c) {
                break;
            }
            match c {
                '\\' if matches!(self.peek_at(1), Some('\n' | '\r')) => break,
                '\'' => {
                    flush(&mut bare, &mut parts);
                    parts.push(WordPart::Quoted(self.single_quoted()?));
                }
                '"' => {
                    flush(&mut bare, &mut parts);
                    let string = self.double_quoted()?;
                    // A string that interpolates gives its text as a
                    // whole, whatever the kinds of the values in it.
                    match string.as_slice() {
                        [] => parts.push(WordPart::Quoted(String::new())),
                        [StringPart::Text(text)] => parts.push(WordPart::Quoted(text.clone())),
                        _ => parts.push(WordPart::Expr(Expr::String(string))),
                    }
                }
                '$' if self
                    .peek_at(1)
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_') =>
                {
                    flush(&mut bare, &mut parts);
                    parts.push(WordPart::Variable(self.variable()?));
                }
                _ => {
                    bare.push(c);
                    self.pos += 1;
                }
            }
        }
        flush(&mut bare, &mut parts);
        if parts.is_empty() {
            return Err(self.error_at(start, "a word is expected here"));
        }
        Ok(Word { parts })
    }

    /// `[` and the text up to its matching `]`.
    fn bracket(&mut self) -> Result<String> {
        let start = self.pos;
        let mut depth = 0;
        loop {
            match self.peek() {
                None | Some('\n') => {
                    return Err(Error {
                        incomplete: self.peek().is_none(),
                        ..self.error_at(start, "this `[` has no matching `]`")
                    });
                }
                Some('[') => depth += 1,
                Some(']') => {
                    depth -= 1;
                    if depth == 0 {
                        self.pos += 1;
                        return Ok(self.chars[start..self.pos].iter().collect());
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
    }

    /// `$name`, `$name.field.0`, `$env.NAME` or `$list...`.
    pub(super) fn variable(&mut self) -> Result<Variable> {
        let mut variable = self.variable_path()?;
        if self.looking_at("...") {
            self.pos += 3;
            variable.spread = true;
        }
        Ok(variable)
    }

    /// `$name`, `$name.field.0` or `$env.NAME`, without `...`.
    fn variable_path(&mut self) -> Result<Variable> {
        self.expect('$', "`$`")?;
        let name = self.variable_name()?;
        let (environment, name) = match name.as_str() {
            "env" if self.peek() == Some('.') => {
                self.pos += 1;
                (true, self.variable_name()?)
            }
            _ => (false, name),
        };
        Ok(Variable {
            environment,
            name,
            path: self.path()?,
            spread: false,
        })
    }

    /// The `.name` and `.0` accesses that follow a variable's name.
    pub(super) fn path(&mut self) -> Result<Vec<Access>> {
        let mut path = Vec::new();
        while self.peek() == Some('.') && !self.looking_at("...") {
            match self.peek_at(1) {
                Some(c) if c.is_ascii_digit() => {
                    self.pos += 1;
                    let digits = self.take_while(|c| c.is_ascii_digit());
                    let index = digits
                        .parse()
                        .map_err(|_| self.error("the index is too large"))?;
                    path.push(Access::Item(index));
                }
                Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                    self.pos += 1;
                    path.push(Access::Field(self.variable_name()?));
                }
                _ => break,
            }
        }
        Ok(path)
    }

    /// A variable's name: letters, digits and `_`.
    fn variable_name(&mut self) -> Result<String> {
        match self.peek() {
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                Ok(self.take_while(|c| c.is_ascii_alphanumeric() || c == '_'))
            }
            _ => Err(self.error("a variable's name is expected here")),
        }
    }

    /// A single-quoted string, which has no escapes.
    pub(super) fn single_quoted(&mut self) -> Result<String> {
        self.pos += 1;
        let mut text = String::new();
        loop {
            match self.peek() {
                None => return Err(self.incomplete("the closing `'` is missing")),
                Some('\'') => {
                    self.pos += 1;
                    return Ok(text);
                }
                Some(c) => {
                    text.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    /// A double-quoted string, with its escapes, and its `{expr}` and
    /// `$name` interpolations; a `$` that no name follows is text, and
    /// `...` after a name is too.
    pub(super) fn double_quoted(&mut self) -> Result<Vec<StringPart>> {
        self.pos += 1;
        let mut parts = Vec::new();
        let mut text = String::new();
        loop {
            match self.peek() {
                None => return Err(self.incomplete("the closing `\"` is missing")),
                Some('"') => {
                    self.pos += 1;
                    break;
                }
                Some('\\') => {
                    let at = self.pos;
                    self.pos += 1;
                    let escaped = match self.peek() {
                        Some('n') => '\n',
                        Some('t') => '\t',
                        Some('r') => '\r',
                        Some('0') => '\0',
                        Some(c @ ('"' | '\\' | '$' | '{' | '}')) => c,
                        Some('u') => {
                            self.pos += 1;
                            self.expect('{', "`{` after `\\u`")?;
                            let hex = self.take_while(|c| c.is_ascii_hexdigit());
                            if hex.is_empty() || hex.len() > 6 || self.peek() != Some('}') {
                                return Err(self
                                    .error_at(at, "`\\u{…}` needs one to six hexadecimal digits"));
                            }
                            let code = u32::from_str_radix(&hex, 16).unwrap_or(u32::MAX);
                            let c = char::from_u32(code)
                                .ok_or_else(|| self.error_at(at, "not a Unicode scalar value"))?;
                            text.push(c);
                            self.pos += 1;
                            continue;
                        }
                        None => return Err(self.incomplete("the closing `\"` is missing")),
                        Some(other) => {
                            return Err(self.error_at(at, &format!("`\\{other}` is not an escape")));
                        }
                    };
                    text.push(escaped);
                    self.pos += 1;
                }
                Some('$')
                    if self
                        .peek_at(1)
                        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_') =>
                {
                    if !text.is_empty() {
                        parts.push(StringPart::Text(std::mem::take(&mut text)));
                    }
                    parts.push(StringPart::Expr(Expr::Variable(self.variable_path()?)));
                }
                Some('{') => {
                    if !text.is_empty() {
                        parts.push(StringPart::Text(std::mem::take(&mut text)));
                    }
                    self.pos += 1;
                    self.skip_blank();
                    let expr = self.expression()?;
                    self.skip_blank();
                    self.expect('}', "`}` after the interpolation")?;
                    parts.push(StringPart::Expr(expr));
                }
                Some(c) => {
                    text.push(c);
                    self.pos += 1;
                }
            }
        }
        if !text.is_empty() {
            parts.push(StringPart::Text(text));
        }
        Ok(parts)
    }
}
