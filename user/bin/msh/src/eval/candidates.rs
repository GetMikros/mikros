//! What completion runs on Tab: the pipeline of a `from` type, whose
//! value's items or output's lines are the candidates, and a program
//! declared with `complete`, run with `MSH_COMPLETE=1` and the words
//! after its name, which writes one candidate on each line. Both run
//! without the terminal, reading nothing, and give nothing once they
//! take longer than `COMPLETION_TIME`. The shell's status, line and
//! command stay as they were.

use super::command::SUDO;
use super::{Input, Run, Shell, System, builtins};
use crate::edit::complete::Request;
use crate::value::{Value, scalar_text};
use std::time::{Duration, Instant};

/// How long what completion runs may take.
pub const COMPLETION_TIME: Duration = Duration::from_secs(1);

/// The environment variable that tells a program to write candidates.
const COMPLETE: &str = "MSH_COMPLETE";

/// The candidates that `value`, a `from` type's, gives: the items of a
/// list, or the lines of text.
fn lines_of(value: &Value) -> Vec<String> {
    let lines: Vec<String> = match value {
        Value::Null => Vec::new(),
        Value::List(items) => items.iter().map(scalar_text).collect(),
        Value::String(text) => text.lines().map(str::to_owned).collect(),
        other => vec![scalar_text(other)],
    };
    lines.into_iter().filter(|l| !l.is_empty()).collect()
}

impl<S: System> Shell<S> {
    /// The lines that `request` gives, none when it fails or takes too
    /// long. Those of a `from cached` type are kept for the line being
    /// edited.
    pub(super) fn completion_lines(&mut self, request: &Request) -> Vec<String> {
        match request {
            Request::From(source) => {
                if source.cached
                    && let Some(lines) = self.cached.get(&source.text)
                {
                    return lines.clone();
                }
                let ran = self.completing(|s| s.eval(&source.expr).ok().map(|v| lines_of(&v)));
                let Some(Some(lines)) = ran else {
                    return Vec::new();
                };
                if source.cached {
                    self.cached.insert(source.text.clone(), lines.clone());
                }
                lines
            }
            Request::Program { name, words } => self.complete_program(name, words),
        }
    }

    /// What the program `name` writes in completion mode with `words`;
    /// nothing for a function or built-in command of that name.
    fn complete_program(&mut self, name: &str, words: &[String]) -> Vec<String> {
        let shadowed = self.functions.contains_key(name) || builtins::find(name).is_some();
        if shadowed && name != SUDO {
            return Vec::new();
        }
        let Ok(mut program) = self.program(name, words.to_vec()) else {
            return Vec::new();
        };
        program.environment =
            self.program_environment(&[(COMPLETE.into(), Value::String("1".into()))]);
        let run = Run {
            text: program.args.join(" "),
            programs: vec![program],
            input: Input::Bytes(Vec::new()),
            capture: true,
            background: false,
            deadline: Some(Instant::now() + COMPLETION_TIME),
        };
        match self.system.run(run) {
            Ok(ran) => String::from_utf8_lossy(&ran.output)
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Runs `f` as completion runs a pipeline: what it writes is
    /// gathered, its programs have until the deadline, and so do its
    /// statements; `None` when it took longer.
    fn completing<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> Option<T> {
        let deadline = Instant::now() + COMPLETION_TIME;
        let (status, line, command) = (self.status(), self.line, self.command.clone());
        let before = self.deadline.replace(deadline);
        self.captures.push(Vec::new());
        let result = f(self);
        self.captures.pop();
        self.deadline = before;
        self.set_global("status", Value::Int(status));
        (self.line, self.command) = (line, command);
        (Instant::now() <= deadline).then_some(result)
    }

    /// Fails once the deadline of what completion runs has passed.
    pub(super) fn in_time(&self) -> Result<(), super::Flow> {
        match self.deadline {
            Some(deadline) if Instant::now() > deadline => {
                Err(self.fail("completion took too long".into()))
            }
            _ => Ok(()),
        }
    }

    /// Fails while completion runs a pipeline, for the built-in command
    /// `name`, which needs the terminal.
    pub(super) fn needs_terminal(&self, name: &str) -> Result<(), super::Flow> {
        match self.deadline {
            Some(_) => Err(self.fail(format!("`{name}` needs the terminal"))),
            None => Ok(()),
        }
    }

    /// Forgets what completion kept for the line edited before.
    pub(super) fn forget_candidates(&mut self) {
        self.cached.clear();
        self.things.borrow_mut().clear();
    }
}
