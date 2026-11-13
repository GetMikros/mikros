//! Running msh's syntax tree: statements, expressions, variables and the
//! environment, with the system behind the `System` trait.

pub mod builtins;
pub mod command;
pub mod glob;
pub mod listings;
pub mod params;
pub mod pattern;
mod sources;
pub mod system;
pub mod text;
pub mod words;

use crate::ast::*;
use crate::value::{self, Value, scalar_text};
use std::collections::BTreeMap;
use std::rc::Rc;

pub use system::{Input, Program, Ran, Run, Stream, System};

/// The environment variables whose values are lists of paths, joined
/// with `:` for programs.
const PATH_LISTS: [&str; 1] = ["PATH"];

/// An error as msh reports and `catch` receives it.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub message: String,
    /// The program's exit status, or 1 for a built-in command.
    pub status: i64,
    pub command: String,
    pub line: usize,
}

impl Failure {
    /// The error as a record, for `catch`.
    pub fn value(&self) -> Value {
        Value::Record(vec![
            ("message".into(), Value::String(self.message.clone())),
            ("status".into(), Value::Int(self.status)),
            ("command".into(), Value::String(self.command.clone())),
            ("line".into(), Value::Int(self.line as i64)),
        ])
    }
}

/// Why statements stop running before their end.
#[derive(Clone, Debug, PartialEq)]
pub enum Flow {
    Error(Failure),
    /// `return` from a function.
    Return(Value),
    /// `exit` from the shell.
    Exit(i64),
}

/// A scope's variables, and whether a function's call starts it, which
/// hides the scopes of the caller.
#[derive(Default)]
struct Frame {
    variables: BTreeMap<String, Value>,
    function: bool,
}

/// A shell: its variables, environment and functions, and the system it
/// runs programs on.
pub struct Shell<S: System> {
    pub system: S,
    frames: Vec<Frame>,
    environment: BTreeMap<String, Value>,
    functions: BTreeMap<String, Rc<Function>>,
    /// Whether statement values are shown, as at the prompt.
    pub interactive: bool,
    /// The script's name, for error messages.
    pub script: Option<String>,
    /// The line and command running, for errors.
    line: usize,
    command: String,
    /// The output of the functions running in a pipeline, innermost
    /// last.
    captures: Vec<Vec<u8>>,
    /// The entries made at the prompt, oldest first, for `history`.
    pub history: Vec<crate::edit::history::Entry>,
    /// The folder listings kept for completion and suggestions.
    listings: listings::Listings,
}

impl<S: System> Shell<S> {
    /// A shell on `system` with the environment `environment`, whose
    /// `PATH` becomes a list.
    pub fn new(system: S, environment: impl IntoIterator<Item = (String, String)>) -> Shell<S> {
        let environment = environment
            .into_iter()
            .map(|(name, value)| {
                let value = match PATH_LISTS.contains(&name.as_str()) {
                    true => Value::List(
                        value
                            .split(':')
                            .filter(|p| !p.is_empty())
                            .map(|p| Value::String(p.into()))
                            .collect(),
                    ),
                    false => Value::String(value),
                };
                (name, value)
            })
            .collect();
        let mut shell = Shell {
            system,
            frames: vec![Frame::default()],
            environment,
            functions: BTreeMap::new(),
            interactive: false,
            script: None,
            line: 0,
            command: String::new(),
            captures: Vec::new(),
            history: Vec::new(),
            listings: listings::Listings::default(),
        };
        shell.set_global("status", Value::Int(0));
        shell.set_global("args", Value::List(Vec::new()));
        shell
    }

    pub fn set_global(&mut self, name: &str, value: Value) {
        self.frames[0].variables.insert(name.into(), value);
    }

    /// An error about the command running, at its line.
    pub fn fail(&self, message: String) -> Flow {
        Flow::Error(Failure {
            message,
            status: 1,
            command: self.command.clone(),
            line: self.line,
        })
    }

    /// The frames that `name` may be in: those up to the innermost
    /// function's, and the global one.
    fn visible(&self) -> impl Iterator<Item = usize> + '_ {
        let boundary = self.frames.iter().rposition(|f| f.function).unwrap_or(0);
        (boundary..self.frames.len())
            .rev()
            .chain((boundary > 0).then_some(0))
    }

    /// The value of `$status`.
    pub fn status(&self) -> i64 {
        match self.lookup("status") {
            Some(Value::Int(n)) => *n,
            _ => 0,
        }
    }

    fn lookup(&self, name: &str) -> Option<&Value> {
        self.visible()
            .find_map(|i| self.frames[i].variables.get(name))
    }

    fn lookup_mut(&mut self, name: &str) -> Option<&mut Value> {
        let at = self
            .visible()
            .find(|&i| self.frames[i].variables.contains_key(name))?;
        self.frames[at].variables.get_mut(name)
    }

    /// Defines `name` in the innermost scope.
    pub fn define(&mut self, name: &str, value: Value) {
        self.frames
            .last_mut()
            .expect("the global scope")
            .variables
            .insert(name.into(), value);
    }

    /// Runs `f` in a new scope, of a function's call when `function`.
    pub fn scoped<T>(&mut self, function: bool, f: impl FnOnce(&mut Self) -> T) -> T {
        self.frames.push(Frame {
            variables: BTreeMap::new(),
            function,
        });
        let result = f(self);
        self.frames.pop();
        result
    }

    /// An environment variable's value.
    pub fn env(&self, name: &str) -> Option<&Value> {
        self.environment.get(name)
    }

    /// Sets an environment variable, or removes it for `null`.
    pub fn set_env(&mut self, name: &str, value: Value) {
        match value {
            Value::Null => {
                self.environment.remove(name);
            }
            value => {
                self.environment.insert(name.into(), value);
            }
        }
    }

    /// The environment as programs get it, with `extra` set too.
    pub fn program_environment(&self, extra: &[(String, Value)]) -> Vec<(String, String)> {
        let mut all = self.environment.clone();
        for (name, value) in extra {
            all.insert(name.clone(), value.clone());
        }
        all.into_iter()
            .map(|(name, value)| {
                let text = match &value {
                    Value::List(items) => {
                        items.iter().map(scalar_text).collect::<Vec<_>>().join(":")
                    }
                    other => scalar_text(other),
                };
                (name, text)
            })
            .collect()
    }

    /// The value of `$name` with its fields and items.
    fn variable(&self, v: &Variable) -> Result<Value, Flow> {
        let mut value = match v.environment {
            true => self
                .env(&v.name)
                .cloned()
                .ok_or_else(|| self.fail(format!("`$env.{}` is not set", v.name)))?,
            false => self
                .lookup(&v.name)
                .cloned()
                .ok_or_else(|| self.fail(format!("`${}` is not set", v.name)))?,
        };
        for access in &v.path {
            value = self.access(&value, access)?;
        }
        Ok(value)
    }

    fn access(&self, value: &Value, access: &Access) -> Result<Value, Flow> {
        let got = match access {
            Access::Field(name) => value.field(name),
            Access::Item(index) => value.item(*index),
        };
        got.cloned().map_err(|e| self.fail(e))
    }

    /// Whether `$name` or `$env.NAME` is set, with its fields and items.
    fn is_set(&self, v: &Variable) -> bool {
        self.variable(v).is_ok()
    }

    /// The value of `expr`.
    pub fn eval(&mut self, expr: &Expr) -> Result<Value, Flow> {
        Ok(match expr {
            Expr::Literal(literal) => Value::from(literal),
            Expr::String(parts) => {
                let mut text = String::new();
                for part in parts {
                    match part {
                        StringPart::Text(t) => text.push_str(t),
                        StringPart::Expr(e) => text.push_str(&scalar_text(&self.eval(e)?)),
                    }
                }
                Value::String(text)
            }
            Expr::Variable(v) => {
                if v.spread {
                    return Err(self.fail("`...` expands arguments, not expressions".into()));
                }
                self.variable(v)?
            }
            Expr::List(items) => {
                let mut values = Vec::new();
                for item in items {
                    match item {
                        Expr::Words(words) => {
                            for word in words {
                                values.extend(self.expand(word)?);
                            }
                        }
                        other => values.push(self.eval(other)?),
                    }
                }
                Value::List(values)
            }
            Expr::Record(fields) => {
                let mut values = Vec::new();
                for (key, e) in fields {
                    values.push((key.clone(), self.eval(e)?));
                }
                Value::Record(values)
            }
            Expr::Unary(op, operand) => {
                let v = self.eval(operand)?;
                value::unary(*op, &v).map_err(|e| self.fail(e))?
            }
            Expr::Binary(Binary::And, a, b) => {
                let a = self.eval(a)?;
                match a.truth().map_err(|e| self.fail(e))? {
                    false => Value::Bool(false),
                    true => Value::Bool(self.eval(b)?.truth().map_err(|e| self.fail(e))?),
                }
            }
            Expr::Binary(Binary::Or, a, b) => {
                let a = self.eval(a)?;
                match a.truth().map_err(|e| self.fail(e))? {
                    true => Value::Bool(true),
                    false => Value::Bool(self.eval(b)?.truth().map_err(|e| self.fail(e))?),
                }
            }
            Expr::Binary(Binary::Default, a, b) => {
                let unset = matches!(&**a, Expr::Variable(v) if !self.is_set(v));
                match unset {
                    true => self.eval(b)?,
                    false => match self.eval(a)? {
                        Value::Null => self.eval(b)?,
                        value => value,
                    },
                }
            }
            Expr::Binary(op, a, b) => {
                let (a, b) = (self.eval(a)?, self.eval(b)?);
                value::binary(*op, &a, &b).map_err(|e| self.fail(e))?
            }
            Expr::Access(e, access) => {
                let value = self.eval(e)?;
                self.access(&value, access)?
            }
            Expr::Pipeline(p) => self.pipeline(p, true)?,
            Expr::Block(_) => return Err(self.fail("a block is not a value".into())),
            Expr::Words(words) => {
                let mut values = Vec::new();
                for word in words {
                    values.extend(self.expand(word)?);
                }
                Value::List(values)
            }
            Expr::Propagate(e) => match self.eval(e) {
                Err(Flow::Error(failure)) if self.in_function() => {
                    return Err(Flow::Return(failure.value()));
                }
                other => other?,
            },
        })
    }

    /// Whether a function is running.
    fn in_function(&self) -> bool {
        self.frames.iter().any(|f| f.function)
    }

    /// Runs `block`'s statements, and gives the value of the last when it
    /// is an expression or a pipeline, and `null` otherwise.
    pub fn block(&mut self, block: &Block) -> Result<Value, Flow> {
        let mut last = Value::Null;
        for statement in &block.statements {
            last = self.statement(statement)?;
        }
        Ok(last)
    }

    /// Runs the statements of a script or a line at the prompt, showing
    /// each statement's value at the prompt unless it is `null`.
    pub fn top(&mut self, block: &Block) -> Result<(), Flow> {
        self.collect_functions(block);
        for statement in &block.statements {
            let value = self.statement(statement)?;
            if self.interactive && value != Value::Null {
                let text = value::prompt_text(&value);
                self.write(Stream::Output, text.as_bytes());
            }
        }
        Ok(())
    }

    /// Defines the functions of `block`, so that they may be called
    /// before their definition.
    fn collect_functions(&mut self, block: &Block) {
        for statement in &block.statements {
            if let StatementKind::Fn(f) = &statement.kind {
                self.functions.insert(f.name.clone(), Rc::new(f.clone()));
            }
        }
    }

    /// Writes to the shell's output, or the running function's.
    pub fn write(&mut self, stream: Stream, bytes: &[u8]) {
        match (stream, self.captures.last_mut()) {
            (Stream::Output, Some(capture)) => capture.extend_from_slice(bytes),
            _ => self.system.write(stream, bytes),
        }
    }

    fn statement(&mut self, statement: &Statement) -> Result<Value, Flow> {
        self.line = statement.line;
        self.command.clear();
        match &statement.kind {
            StatementKind::Let { name, value } => {
                let value = self.eval(value)?;
                self.define(name, value);
                Ok(Value::Null)
            }
            StatementKind::Assign {
                target,
                append,
                value,
            } => {
                let value = self.eval(value)?;
                self.assign(target, *append, value)?;
                Ok(Value::Null)
            }
            StatementKind::Pipeline(p) => self.pipeline(p, false),
            StatementKind::Expr(e) => self.eval(e),
            StatementKind::If {
                condition,
                then,
                otherwise,
            } => {
                if self.condition(condition)? {
                    self.scoped(false, |s| s.block(then))?;
                } else if let Some(otherwise) = otherwise {
                    self.scoped(false, |s| s.block(otherwise))?;
                }
                Ok(Value::Null)
            }
            StatementKind::While { condition, body } => {
                while self.condition(condition)? {
                    self.scoped(false, |s| s.block(body))?;
                }
                Ok(Value::Null)
            }
            StatementKind::For { name, items, body } => {
                let items = match self.eval(items)? {
                    Value::List(items) => items,
                    other => {
                        return Err(
                            self.fail(format!("`for` takes a list, not a {}", other.kind()))
                        );
                    }
                };
                for item in items {
                    self.scoped(false, |s| {
                        s.define(name, item);
                        s.block(body)
                    })?;
                }
                Ok(Value::Null)
            }
            StatementKind::Match { value, arms } => {
                let value = self.eval(value)?;
                for arm in arms {
                    let mut bound = Vec::new();
                    if !pattern::matches(&arm.pattern, &value, &mut bound) {
                        continue;
                    }
                    let ran = self.scoped(false, |s| {
                        for (name, v) in bound {
                            s.define(&name, v);
                        }
                        if let Some(guard) = &arm.guard {
                            let ok = s.eval(guard)?;
                            if !ok.truth().map_err(|e| s.fail(e))? {
                                return Ok(None);
                            }
                        }
                        s.block(&arm.body).map(Some)
                    })?;
                    if let Some(value) = ran {
                        return Ok(value);
                    }
                }
                Err(self.fail(format!("no arm of `match` matches {}", scalar_text(&value))))
            }
            StatementKind::Fn(f) => {
                self.functions.insert(f.name.clone(), Rc::new(f.clone()));
                Ok(Value::Null)
            }
            // A script's options are checked and set before it runs.
            StatementKind::Param(_) => Ok(Value::Null),
            StatementKind::Try {
                body,
                name,
                handler,
            } => {
                match self.scoped(false, |s| s.block(body)) {
                    Err(Flow::Error(failure)) => {
                        self.scoped(false, |s| {
                            s.define(name, failure.value());
                            s.block(handler)
                        })?;
                    }
                    other => {
                        other?;
                    }
                }
                Ok(Value::Null)
            }
            StatementKind::Use(word) => {
                let path = self.expand_one(word)?;
                let source = self.system.read_file(&path).map_err(|e| self.fail(e))?;
                let block =
                    crate::parse::parse(&source).map_err(|e| self.fail(format!("{path}: {e}")))?;
                self.collect_functions(&block);
                Ok(Value::Null)
            }
            StatementKind::Return(value) => {
                let value = match value {
                    Some(e) => self.eval(e)?,
                    None => Value::Null,
                };
                Err(Flow::Return(value))
            }
        }
    }

    fn assign(&mut self, target: &Target, append: bool, value: Value) -> Result<(), Flow> {
        let current = match target {
            Target::Variable(name) => self.lookup(name).cloned(),
            Target::Environment(name) => self.env(name).cloned(),
        };
        let new = match (append, current) {
            (false, Some(_)) => value,
            (false, None) if matches!(target, Target::Environment(_)) => value,
            (_, None) => {
                let name = match target {
                    Target::Variable(n) => format!("${n}"),
                    Target::Environment(n) => format!("$env.{n}"),
                };
                return Err(self.fail(format!("`{name}` is not set; `let` creates it")));
            }
            (true, Some(Value::List(mut items))) => {
                items.push(value);
                Value::List(items)
            }
            (true, Some(Value::String(text))) => match value {
                Value::String(more) => Value::String(text + &more),
                other => {
                    return Err(
                        self.fail(format!("`+=` cannot add a {} to a string", other.kind()))
                    );
                }
            },
            (true, Some(other)) => {
                return Err(self.fail(format!(
                    "`+=` appends to a list or a string, not a {}",
                    other.kind()
                )));
            }
        };
        match target {
            Target::Variable(name) => {
                *self.lookup_mut(name).expect("looked up") = new;
            }
            Target::Environment(name) => self.set_env(name, new),
        }
        Ok(())
    }

    /// Whether the condition of `if` or `while` holds: a boolean
    /// expression, or a command that exits with 0, whose failure does
    /// not stop the script.
    fn condition(&mut self, condition: &Condition) -> Result<bool, Flow> {
        match condition {
            Condition::Expr(e) => {
                let value = self.eval(e)?;
                value.truth().map_err(|e| self.fail(e))
            }
            Condition::Command(p) => match self.pipeline(p, false) {
                Ok(_) => Ok(true),
                Err(Flow::Error(_)) => Ok(false),
                Err(other) => Err(other),
            },
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
