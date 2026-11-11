//! Pipelines: finding each command, starting programs together with
//! pipes between them, and passing values between built-in commands and
//! functions.

use super::{Flow, Input, Program, Ran, Run, Shell, Stream, System, builtins};
use crate::ast::*;
use crate::jobs::Ended;
use crate::value::{Value, program_text, scalar_text};
use std::rc::Rc;

/// What a pipeline carries from one command to the next.
enum Carry {
    /// Nothing: the first command reads the shell's input.
    Nothing,
    Value(Value),
    Bytes(Vec<u8>),
}

/// A command found and its arguments expanded.
enum Found {
    Expr(Value),
    Function(Rc<Function>, Vec<Value>),
    Builtin(&'static str, Vec<builtins::Arg>),
    Program(Program),
}

/// The status a command that is not found gives, and one that cannot be
/// run.
const NOT_FOUND: i64 = 127;
const NOT_EXECUTABLE: i64 = 126;

impl<S: System> Shell<S> {
    /// Runs pipeline `p`. Its last command's value is the pipeline's; a
    /// last program's output is written to the shell's output, or, when
    /// `capture`, given as a string. `$status` is set from how it ended.
    pub fn pipeline(&mut self, p: &Pipeline, capture: bool) -> Result<Value, Flow> {
        let result = self.run_pipeline(p, capture);
        let status = match &result {
            Ok(_) => 0,
            Err(Flow::Error(failure)) => failure.status,
            Err(_) => return result,
        };
        self.set_global("status", Value::Int(status));
        result
    }

    fn run_pipeline(&mut self, p: &Pipeline, capture: bool) -> Result<Value, Flow> {
        let mut found = Vec::new();
        for command in &p.commands {
            found.push(self.find(command)?);
        }
        if p.background {
            if !found.iter().all(|f| matches!(f, Found::Program(_))) {
                return Err(self.fail("only programs run in the background".into()));
            }
            let programs = found
                .into_iter()
                .map(|f| match f {
                    Found::Program(p) => p,
                    _ => unreachable!("all programs"),
                })
                .collect();
            let run = Run {
                programs,
                input: Input::Inherit,
                capture: false,
                background: true,
                text: self.command.clone(),
            };
            self.system.run(run).map_err(|e| self.fail(e))?;
            return Ok(Value::Null);
        }
        let count = found.len();
        let mut carry = Carry::Nothing;
        let mut failure = None;
        let mut programs: Vec<Program> = Vec::new();
        for (index, f) in found.into_iter().enumerate() {
            let last = index + 1 == count;
            match f {
                Found::Program(program) => {
                    programs.push(program);
                    if last {
                        carry = self.start(
                            std::mem::take(&mut programs),
                            carry,
                            capture,
                            &mut failure,
                        )?;
                    }
                }
                other => {
                    if !programs.is_empty() {
                        carry =
                            self.start(std::mem::take(&mut programs), carry, true, &mut failure)?;
                    }
                    let input = match std::mem::replace(&mut carry, Carry::Nothing) {
                        Carry::Nothing => None,
                        Carry::Value(v) => Some(v),
                        Carry::Bytes(b) => {
                            Some(Value::String(String::from_utf8_lossy(&b).into_owned()))
                        }
                    };
                    carry = match self.call(other, input, !last || capture) {
                        Ok(value) => Carry::Value(value),
                        Err(Flow::Error(f)) => {
                            failure = Some(f);
                            Carry::Value(Value::Null)
                        }
                        Err(other) => return Err(other),
                    };
                }
            }
        }
        if let Some(failure) = failure {
            return Err(Flow::Error(failure));
        }
        Ok(match carry {
            Carry::Nothing => Value::Null,
            Carry::Value(v) => v,
            Carry::Bytes(b) => Value::String(String::from_utf8_lossy(&b).into_owned()),
        })
    }

    /// Starts `programs` joined by pipes, reading `carry`, and returns
    /// their output when `capture`, or when a function's output is being
    /// gathered. A program that fails becomes `failure`.
    fn start(
        &mut self,
        programs: Vec<Program>,
        carry: Carry,
        capture: bool,
        failure: &mut Option<super::Failure>,
    ) -> Result<Carry, Flow> {
        let input = match carry {
            Carry::Nothing => Input::Inherit,
            Carry::Value(v) => Input::Bytes(program_text(&v).into_bytes()),
            Carry::Bytes(b) => Input::Bytes(b),
        };
        let gathered = !capture && !self.captures.is_empty();
        let names: Vec<String> = programs.iter().map(|p| p.args[0].clone()).collect();
        let run = Run {
            programs,
            input,
            capture: capture || gathered,
            background: false,
            text: self.command.clone(),
        };
        let Ran { ended, output } = self.system.run(run).map_err(|e| self.fail(e))?;
        if let Some(f) = self.ended(&names, &ended) {
            *failure = Some(f);
        }
        if gathered {
            self.write(Stream::Output, &output);
            return Ok(Carry::Nothing);
        }
        Ok(match capture {
            true => Carry::Bytes(output),
            false => Carry::Nothing,
        })
    }

    /// The failure of the last of the programs `names` that did not
    /// exit with 0, as `ended` says.
    pub fn ended(&self, names: &[String], ended: &[Ended]) -> Option<super::Failure> {
        let mut failure = None;
        for (name, ended) in names.iter().zip(ended) {
            let (status, message) = match ended {
                Ended::Exited(0) => continue,
                Ended::Exited(code) => (i64::from(*code), format!("exited with status {code}")),
                Ended::Signaled(signal) => (
                    128 + i64::from(*signal),
                    format!("ended by signal {signal}"),
                ),
            };
            failure = Some(super::Failure {
                message,
                status,
                command: name.clone(),
                line: self.line,
            });
        }
        failure
    }

    /// The program `name` with `args`, as found in `$env.PATH`.
    pub fn program(&self, name: &str, args: Vec<String>) -> Result<Program, Flow> {
        let path = self.locate(name)?;
        let mut all = vec![name.to_owned()];
        all.extend(args);
        Ok(Program {
            path,
            args: all,
            environment: self.program_environment(&[]),
            redirections: Vec::new(),
        })
    }

    /// Finds `command`: an expression's value, a function, a built-in
    /// command or a program, with its arguments expanded.
    fn find(&mut self, command: &Command) -> Result<Found, Flow> {
        let simple = match command {
            Command::Expr(e) => return Ok(Found::Expr(self.eval(e)?)),
            Command::Simple(s) => s,
        };
        let mut args = Vec::new();
        for word in &simple.words {
            match word {
                Argument::Word(w) => {
                    for value in self.expand(w)? {
                        args.push(builtins::Arg::Value(value));
                    }
                }
                Argument::Block(b) => args.push(builtins::Arg::Block(b.clone())),
            }
        }
        // `run name args…` runs the command whose name is a value.
        if matches!(args.first(), Some(builtins::Arg::Value(Value::String(n))) if n == "run") {
            if args.len() < 2 {
                return Err(self.fail("`run` takes a command's name".into()));
            }
            args.remove(0);
        }
        let name = match args.first() {
            Some(builtins::Arg::Value(v)) => scalar_text(v),
            _ => return Err(self.fail("a command's name must be a word".into())),
        };
        self.command = name.clone();
        let mut environment = Vec::new();
        for (var, word) in &simple.environment {
            let value = match word.parts.is_empty() {
                true => Value::String(String::new()),
                false => Value::String(self.expand_one(word)?),
            };
            environment.push((var.clone(), value));
        }
        let mut redirections = Vec::new();
        for r in &simple.redirections {
            let target = match r.target.parts.is_empty() {
                true => String::new(),
                false => self.expand_one(&r.target)?,
            };
            redirections.push((r.kind, target));
        }
        let rest = args.split_off(1);
        if let Some(f) = self.functions.get(&name).cloned() {
            self.only_programs(&environment, &redirections)?;
            let values = rest
                .into_iter()
                .map(|a| match a {
                    builtins::Arg::Value(v) => Ok(v),
                    builtins::Arg::Block(_) => Err(self.fail("a function takes no blocks".into())),
                })
                .collect::<Result<_, _>>()?;
            return Ok(Found::Function(f, values));
        }
        if let Some(builtin) = builtins::find(&name) {
            self.only_programs(&environment, &redirections)?;
            return Ok(Found::Builtin(builtin, rest));
        }
        let path = self.locate(&name)?;
        let mut program_args = vec![name];
        for arg in rest {
            match arg {
                builtins::Arg::Value(v) => program_args.push(scalar_text(&v)),
                builtins::Arg::Block(_) => {
                    return Err(self.fail("a program takes no blocks".into()));
                }
            }
        }
        Ok(Found::Program(Program {
            path,
            args: program_args,
            environment: self.program_environment(&environment),
            redirections,
        }))
    }

    /// Fails for `NAME=value` words and redirections, which only programs
    /// take.
    fn only_programs(
        &self,
        environment: &[(String, Value)],
        redirections: &[(RedirectionKind, String)],
    ) -> Result<(), Flow> {
        if !environment.is_empty() || !redirections.is_empty() {
            return Err(self.fail("only programs take NAME=value words and redirections".into()));
        }
        Ok(())
    }

    /// The path of program `name`: the file it names when it holds a
    /// `/`, and otherwise the first that may be run in `$env.PATH`'s
    /// folders.
    fn locate(&self, name: &str) -> Result<String, Flow> {
        let not_found = |status: i64, message: &str| {
            let Flow::Error(mut failure) = self.fail(message.into()) else {
                unreachable!("fail gives an error")
            };
            failure.status = status;
            Flow::Error(failure)
        };
        if name.contains('/') {
            return match self.system.executable(name) {
                Some(true) => Ok(name.into()),
                Some(false) => Err(not_found(NOT_EXECUTABLE, "permission denied")),
                None => Err(not_found(NOT_FOUND, "command not found")),
            };
        }
        let folders: Vec<String> = match self.env("PATH") {
            Some(Value::List(items)) => items.iter().map(scalar_text).collect(),
            Some(other) => vec![scalar_text(other)],
            None => Vec::new(),
        };
        let mut denied = false;
        for folder in folders {
            let path = format!("{}/{name}", folder.trim_end_matches('/'));
            match self.system.executable(&path) {
                Some(true) => return Ok(path),
                Some(false) => denied = true,
                None => {}
            }
        }
        Err(match denied {
            true => not_found(NOT_EXECUTABLE, "permission denied"),
            false => not_found(NOT_FOUND, "command not found"),
        })
    }

    /// Calls a function or built-in command with `input`, gathering the
    /// output of the programs it starts when `gather`.
    fn call(&mut self, found: Found, input: Option<Value>, gather: bool) -> Result<Value, Flow> {
        match found {
            Found::Expr(value) => match input {
                None => Ok(value),
                Some(_) => Err(self.fail("an expression takes no input from a pipe".into())),
            },
            Found::Builtin(name, args) => builtins::run(self, name, args, input),
            Found::Function(f, args) => {
                if gather {
                    self.captures.push(Vec::new());
                }
                let result = self.function(&f, args);
                let output = match gather {
                    true => self.captures.pop().unwrap_or_default(),
                    false => Vec::new(),
                };
                let value = result?;
                Ok(match (value, output.is_empty()) {
                    (Value::Null, false) => {
                        Value::String(String::from_utf8_lossy(&output).into_owned())
                    }
                    (value, _) => value,
                })
            }
            Found::Program(_) => unreachable!("programs are started, not called"),
        }
    }

    /// Calls function `f` with `args`: its arguments in order, and its
    /// flags as `--name value`, `--name=value` or `--name` for `true`.
    fn function(&mut self, f: &Function, args: Vec<Value>) -> Result<Value, Flow> {
        let mut positional = Vec::new();
        let mut flags: Vec<(String, Value)> = Vec::new();
        let mut args = args.into_iter().peekable();
        while let Some(arg) = args.next() {
            let flag = match &arg {
                Value::String(s) if s.starts_with("--") && s.len() > 2 => s[2..].to_owned(),
                _ => {
                    positional.push(arg);
                    continue;
                }
            };
            let (name, value) = match flag.split_once('=') {
                Some((name, value)) => (name.to_owned(), Value::String(value.into())),
                None => {
                    let takes_value = !matches!(
                        f.flags.iter().find(|(n, _)| *n == flag),
                        Some((_, Some(Expr::Literal(Literal::Bool(_)))))
                    );
                    match (takes_value, args.peek()) {
                        (true, Some(_)) => (flag, args.next().expect("peeked")),
                        _ => (flag, Value::Bool(true)),
                    }
                }
            };
            if !f.flags.iter().any(|(n, _)| *n == name) {
                return Err(self.fail(format!("`{}` has no flag `--{name}`", f.name)));
            }
            flags.push((name, value));
        }
        if positional.len() != f.arguments.len() {
            return Err(self.fail(format!(
                "`{}` takes {} arguments, not {}",
                f.name,
                f.arguments.len(),
                positional.len()
            )));
        }
        let mut defaults = Vec::new();
        for (name, default) in &f.flags {
            if flags.iter().any(|(n, _)| n == name) {
                continue;
            }
            let value = match default {
                Some(e) => self.eval(e)?,
                None => Value::Null,
            };
            defaults.push((name.clone(), value));
        }
        let line = self.line;
        let result = self.scoped(true, |s| {
            for (name, value) in f
                .arguments
                .iter()
                .cloned()
                .zip(positional)
                .chain(flags)
                .chain(defaults)
            {
                s.define(&name, value);
            }
            s.block(&f.body)
        });
        self.line = line;
        match result {
            Err(Flow::Return(value)) => Ok(value),
            other => other,
        }
    }
}
