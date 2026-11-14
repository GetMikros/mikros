//! Expanding words into arguments: variables, `$list...`, expressions in
//! words, and glob patterns written literally.

use super::glob::{self, Piece};
use super::{Flow, Shell, System};
use crate::ast::{Word, WordPart};
use crate::value::{Value, scalar_text};

impl<S: System> Shell<S> {
    /// The arguments of `word`: a lone `$name` or expression gives its
    /// value, `$list...` one value for each item, a word with wildcards
    /// written outside quotes the paths it matches, and any other word
    /// one string of its parts' texts.
    pub fn expand(&mut self, word: &Word) -> Result<Vec<Value>, Flow> {
        match word.parts.as_slice() {
            [WordPart::Variable(v)] if v.spread => match self.variable(v)? {
                Value::List(items) => return Ok(items),
                other => {
                    return Err(self.fail(format!("`...` takes a list, not a {}", other.kind())));
                }
            },
            [WordPart::Variable(v)] => return Ok(vec![self.variable(v)?]),
            [WordPart::Expr(expr)] => return Ok(vec![self.eval(expr)?]),
            _ => {}
        }
        let mut pieces = Vec::new();
        let mut text = String::new();
        let mut wild = false;
        for part in &word.parts {
            let piece = match part {
                WordPart::Bare(bare) => {
                    wild |= glob::has_wildcards(bare);
                    Piece::Pattern(bare.clone())
                }
                WordPart::Quoted(quoted) => Piece::Literal(quoted.clone()),
                WordPart::Variable(v) => {
                    if v.spread {
                        return Err(self.fail("`$list...` must be a word of its own".into()));
                    }
                    Piece::Literal(scalar_text(&self.variable(v)?))
                }
                WordPart::Expr(expr) => Piece::Literal(scalar_text(&self.eval(expr)?)),
                WordPart::Home(user) => Piece::Literal(self.home(user.as_deref())?),
            };
            match &piece {
                Piece::Pattern(t) | Piece::Literal(t) => text.push_str(t),
            }
            pieces.push(piece);
        }
        if !wild {
            return Ok(vec![Value::String(text)]);
        }
        match glob::expand(&pieces, &self.system) {
            Ok(paths) => Ok(paths.into_iter().map(Value::String).collect()),
            Err(message) => Err(self.fail(message)),
        }
    }

    /// The folder that `~` stands for, `$env.HOME`, or `~name`, the home
    /// folder of the user `name`, which stays as written when there is
    /// no such user.
    fn home(&self, user: Option<&str>) -> Result<String, Flow> {
        match user {
            None => match self.env("HOME") {
                Some(home) => Ok(scalar_text(home)),
                None => Err(self.fail("`$env.HOME` is not set".into())),
            },
            Some(name) => Ok(self
                .system
                .home_of(name)
                .unwrap_or_else(|| format!("~{name}"))),
        }
    }

    /// The text of `word` as one argument, for redirections and `use`.
    pub fn expand_one(&mut self, word: &Word) -> Result<String, Flow> {
        let mut values = self.expand(word)?;
        match values.len() {
            1 => Ok(scalar_text(&values.pop().expect("one"))),
            n => Err(self.fail(format!("the word expands to {n} arguments, not one"))),
        }
    }
}
