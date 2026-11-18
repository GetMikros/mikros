//! Aliases: `alias name = words…` defines one, `alias name` prints it,
//! `alias` lists them all and `unalias name` removes one. The first word
//! of a command in a line typed at the prompt that names an alias is
//! replaced by the alias's words, once, so that the result is not looked
//! up again; scripts, sourced files and functions keep their words.
//! `sudo` cannot be an alias.

use super::command::SUDO;
use super::{Flow, Shell, Stream, System};
use crate::ast::*;
use crate::edit::complete::as_word;
use crate::value::{Value, scalar_text};

/// The aliases an interactive msh defines before it runs `~/.mshrc`.
pub const DEFAULT_ALIASES: [(&str, &[&str]); 2] = [("ls", &["ls", "-l"]), ("la", &["ls", "-la"])];

/// The line that shows the alias `name` for `words`, as it is defined.
fn shown(name: &str, words: &[String]) -> String {
    let words: Vec<String> = words.iter().map(|w| as_word(w)).collect();
    format!("{} = {}\n", as_word(name), words.join(" "))
}

impl<S: System> Shell<S> {
    /// The words that the alias `name` stands for, if there is one.
    pub fn alias_of(&self, name: &str) -> Option<&[String]> {
        self.aliases.get(name).map(Vec::as_slice)
    }

    /// The aliases, by name, with their words.
    pub fn aliases(&self) -> impl Iterator<Item = (&String, &Vec<String>)> {
        self.aliases.iter()
    }

    /// Defines the aliases of `DEFAULT_ALIASES`.
    pub fn define_default_aliases(&mut self) {
        for (name, words) in DEFAULT_ALIASES {
            let words = words.iter().map(|w| w.to_string()).collect();
            self.aliases.insert(name.into(), words);
        }
    }

    /// The built-in command `alias`, with its arguments `values`.
    pub(super) fn alias(&mut self, values: &[Value]) -> Result<Value, Flow> {
        let texts: Vec<String> = values.iter().map(scalar_text).collect();
        match texts.as_slice() {
            [] => {
                let lines: String = self.aliases.iter().map(|(n, w)| shown(n, w)).collect();
                self.write(Stream::Output, lines.as_bytes());
            }
            [name] => {
                let line = match self.aliases.get(name) {
                    Some(words) => shown(name, words),
                    None => return Err(self.fail(format!("`{name}` is no alias"))),
                };
                self.write(Stream::Output, line.as_bytes());
            }
            [name, equals, words @ ..] if equals == "=" => {
                if name == SUDO {
                    return Err(self.fail("sudo cannot be an alias".into()));
                }
                if words.is_empty() {
                    return Err(self.fail("an alias stands for one word at least".into()));
                }
                self.aliases.insert(name.clone(), words.to_vec());
            }
            _ => {
                return Err(self.fail("`alias` takes `name = words…`, a name, or nothing".into()));
            }
        }
        Ok(Value::Null)
    }

    /// The built-in command `unalias`, with its arguments `values`.
    pub(super) fn unalias(&mut self, values: &[Value]) -> Result<Value, Flow> {
        let [name] = values else {
            return Err(self.fail("`unalias` takes the name of an alias".into()));
        };
        let name = scalar_text(name);
        match self.aliases.remove(&name) {
            Some(_) => Ok(Value::Null),
            None => Err(self.fail(format!("`{name}` is no alias"))),
        }
    }

    /// Replaces the first word of each command of `block`, a line typed
    /// at the prompt, that names an alias by the alias's words, apart from
    /// the commands in the functions it defines.
    pub fn expand_aliases(&self, block: &mut Block) {
        if !self.aliases.is_empty() {
            self.expand_block(block);
        }
    }

    fn expand_block(&self, block: &mut Block) {
        for statement in &mut block.statements {
            self.expand_statement(&mut statement.kind);
        }
    }

    fn expand_statement(&self, kind: &mut StatementKind) {
        match kind {
            StatementKind::Let { value, .. } | StatementKind::Assign { value, .. } => {
                self.expand_expr(value)
            }
            StatementKind::Pipeline(p) => self.expand_pipeline(p),
            StatementKind::Expr(e) | StatementKind::Return(Some(e)) => self.expand_expr(e),
            StatementKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expand_condition(condition);
                self.expand_block(then);
                if let Some(otherwise) = otherwise {
                    self.expand_block(otherwise);
                }
            }
            StatementKind::While { condition, body } => {
                self.expand_condition(condition);
                self.expand_block(body);
            }
            StatementKind::For { items, body, .. } => {
                self.expand_expr(items);
                self.expand_block(body);
            }
            StatementKind::Match { value, arms } => {
                self.expand_expr(value);
                for arm in arms {
                    if let Some(guard) = &mut arm.guard {
                        self.expand_expr(guard);
                    }
                    self.expand_block(&mut arm.body);
                }
            }
            StatementKind::Try { body, handler, .. } => {
                self.expand_block(body);
                self.expand_block(handler);
            }
            // A function keeps its words, wherever it is defined.
            StatementKind::Fn(_) => {}
            StatementKind::Param(_)
            | StatementKind::Use(_)
            | StatementKind::Return(None)
            | StatementKind::Delete(_)
            | StatementKind::Extern(_) => {}
        }
    }

    fn expand_condition(&self, condition: &mut Condition) {
        match condition {
            Condition::Expr(e) => self.expand_expr(e),
            Condition::Command(p) => self.expand_pipeline(p),
        }
    }

    fn expand_pipeline(&self, pipeline: &mut Pipeline) {
        for command in &mut pipeline.commands {
            match command {
                Command::Simple(simple) => self.expand_simple(simple),
                Command::Expr(e) => self.expand_expr(e),
            }
        }
    }

    fn expand_simple(&self, simple: &mut Simple) {
        let alias = match simple.words.first() {
            Some(Argument::Word(Word { parts })) => match parts.as_slice() {
                [WordPart::Bare(name)] => self.aliases.get(name),
                _ => None,
            },
            _ => None,
        };
        if let Some(words) = alias {
            let words = words.iter().map(|w| {
                Argument::Word(Word {
                    parts: vec![WordPart::Quoted(w.clone())],
                })
            });
            simple.words.splice(0..1, words);
        }
        for argument in &mut simple.words {
            match argument {
                Argument::Word(word) => self.expand_word(word),
                Argument::Block(block) => self.expand_block(block),
            }
        }
        for (_, word) in &mut simple.environment {
            self.expand_word(word);
        }
        for redirection in &mut simple.redirections {
            self.expand_word(&mut redirection.target);
        }
    }

    fn expand_word(&self, word: &mut Word) {
        for part in &mut word.parts {
            match part {
                WordPart::Expr(e) => self.expand_expr(e),
                WordPart::Variable(v) => self.expand_variable(v),
                WordPart::Bare(_) | WordPart::Quoted(_) | WordPart::Home(_) => {}
            }
        }
    }

    fn expand_variable(&self, variable: &mut Variable) {
        for access in &mut variable.path {
            self.expand_access(access);
        }
    }

    fn expand_access(&self, access: &mut Access) {
        if let Access::Computed(e) = access {
            self.expand_expr(e);
        }
    }

    fn expand_expr(&self, expr: &mut Expr) {
        match expr {
            Expr::Literal(_) => {}
            Expr::String(parts) => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        self.expand_expr(e);
                    }
                }
            }
            Expr::Variable(v) => self.expand_variable(v),
            Expr::List(items) => items.iter_mut().for_each(|e| self.expand_expr(e)),
            Expr::Record(fields) => fields.iter_mut().for_each(|(_, e)| self.expand_expr(e)),
            Expr::Unary(_, e) | Expr::Propagate(e) => self.expand_expr(e),
            Expr::Binary(_, a, b) => {
                self.expand_expr(a);
                self.expand_expr(b);
            }
            Expr::Access(e, access) => {
                self.expand_expr(e);
                self.expand_access(access);
            }
            Expr::Pipeline(p) => self.expand_pipeline(p),
            Expr::Block(block) => self.expand_block(block),
            Expr::Words(words) => words.iter_mut().for_each(|w| self.expand_word(w)),
        }
    }
}
