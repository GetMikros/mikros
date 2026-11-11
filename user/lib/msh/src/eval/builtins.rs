//! Built-in commands: `cd`, `pwd`, `exit`, `print`, `source`, `env`,
//! `jobs`, `fg`, `bg`, `kill`, `exec` and `history`, and those of
//! `text`.

use super::{Flow, Shell, Stream, System};
use crate::ast::Block;
use crate::value::{Value, program_text, scalar_text};

/// An argument of a built-in command: a value, or a block.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Value(Value),
    Block(Block),
}

const BUILTINS: [&str; 12] = [
    "cd", "pwd", "exit", "print", "source", "env", "jobs", "fg", "bg", "kill", "exec", "history",
];

/// The signals `kill` knows by name, with their numbers.
const SIGNALS: [(&str, i32); 10] = [
    ("HUP", 1),
    ("INT", 2),
    ("QUIT", 3),
    ("KILL", 9),
    ("USR1", 10),
    ("USR2", 12),
    ("TERM", 15),
    ("CONT", 18),
    ("STOP", 19),
    ("TSTP", 20),
];

/// The number of signal `name`, a number or a name with or without `SIG`.
fn signal(name: &str) -> Option<i32> {
    if let Ok(n) = name.parse() {
        return Some(n);
    }
    let name = name.strip_prefix("SIG").unwrap_or(name);
    SIGNALS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// The built-in command `name`, if there is one.
pub fn find(name: &str) -> Option<&'static str> {
    BUILTINS
        .iter()
        .chain(super::text::COMMANDS.iter())
        .find(|b| **b == name)
        .copied()
}

impl<S: System> Shell<S> {
    /// The values of `args`, failing for a block.
    fn values(&self, name: &str, args: Vec<Arg>) -> Result<Vec<Value>, Flow> {
        args.into_iter()
            .map(|a| match a {
                Arg::Value(v) => Ok(v),
                Arg::Block(_) => Err(self.fail(format!("`{name}` takes no blocks"))),
            })
            .collect()
    }

    fn at_most(&self, name: &str, values: &[Value], n: usize) -> Result<(), Flow> {
        match values.len() > n {
            true => Err(self.fail(format!("`{name}` takes at most {n} arguments"))),
            false => Ok(()),
        }
    }

    fn env_text(&self, name: &str) -> Option<String> {
        self.env(name).map(scalar_text)
    }
}

/// Runs built-in command `name` with `args` and the pipe's `input`.
pub fn run<S: System>(
    shell: &mut Shell<S>,
    name: &'static str,
    args: Vec<Arg>,
    input: Option<Value>,
) -> Result<Value, Flow> {
    if super::text::COMMANDS.contains(&name) {
        return super::text::run(shell, name, args, input);
    }
    let values = shell.values(name, args)?;
    match name {
        "cd" => {
            shell.at_most(name, &values, 1)?;
            let target = match values.first().map(scalar_text).as_deref() {
                None => shell
                    .env_text("HOME")
                    .ok_or_else(|| shell.fail("`$env.HOME` is not set".into()))?,
                Some("-") => shell
                    .env_text("OLDPWD")
                    .ok_or_else(|| shell.fail("there is no previous folder".into()))?,
                Some(path) => path.to_owned(),
            };
            let old = shell.system.current_dir();
            shell
                .system
                .set_current_dir(&target)
                .map_err(|e| shell.fail(e))?;
            let new = shell.system.current_dir();
            shell.set_env("OLDPWD", Value::String(old));
            shell.set_env("PWD", Value::String(new));
            Ok(Value::Null)
        }
        "pwd" => {
            shell.at_most(name, &values, 0)?;
            let folder = shell.system.current_dir();
            shell.write(Stream::Output, format!("{folder}\n").as_bytes());
            Ok(Value::Null)
        }
        "exit" => {
            shell.at_most(name, &values, 1)?;
            let status = match values.first() {
                None => match shell.lookup("status") {
                    Some(Value::Int(n)) => *n,
                    _ => 0,
                },
                Some(Value::Int(n)) => *n,
                Some(v) => scalar_text(v).parse().map_err(|_| {
                    shell.fail(format!("`exit` takes a status, not `{}`", scalar_text(v)))
                })?,
            };
            Err(Flow::Exit(status))
        }
        "print" => {
            let (newline, values) = match values.first() {
                Some(Value::String(flag)) if flag == "-n" => (false, &values[1..]),
                _ => (true, &values[..]),
            };
            let texts: Vec<String> = input.iter().chain(values).map(text).collect();
            let mut out = texts.join(" ");
            if newline {
                out.push('\n');
            }
            shell.write(Stream::Output, out.as_bytes());
            Ok(Value::Null)
        }
        "source" => {
            if values.len() != 1 {
                return Err(shell.fail("`source` takes a script's path".into()));
            }
            let path = scalar_text(&values[0]);
            let source = shell.system.read_file(&path).map_err(|e| shell.fail(e))?;
            let block =
                crate::parse::parse(&source).map_err(|e| shell.fail(format!("{path}: {e}")))?;
            let (line, command) = (shell.line, shell.command.clone());
            shell.collect_functions(&block);
            let ran = shell.block(&block);
            shell.line = line;
            shell.command = command;
            ran.map(|_| Value::Null)
        }
        "env" => {
            shell.at_most(name, &values, 0)?;
            let rows = shell
                .environment
                .iter()
                .map(|(k, v)| {
                    Value::Record(vec![
                        ("name".into(), Value::String(k.clone())),
                        ("value".into(), v.clone()),
                    ])
                })
                .collect();
            Ok(Value::List(rows))
        }
        "jobs" => {
            shell.at_most(name, &values, 0)?;
            for line in shell.system.jobs() {
                shell.write(Stream::Output, format!("{line}\n").as_bytes());
            }
            Ok(Value::Null)
        }
        "fg" | "bg" => {
            shell.at_most(name, &values, 1)?;
            let id = values.first().map_or_else(|| "%+".to_owned(), scalar_text);
            let ran = shell
                .system
                .resume(&id, name == "fg")
                .map_err(|e| shell.fail(e))?;
            let names = vec![id; ran.ended.len()];
            match shell.ended(&names, &ran.ended) {
                Some(failure) => Err(Flow::Error(failure)),
                None => Ok(Value::Null),
            }
        }
        "kill" => {
            let texts: Vec<String> = values.iter().map(scalar_text).collect();
            let (number, targets) = match texts.as_slice() {
                [flag, sig, rest @ ..] if flag == "-s" => (signal(sig), rest),
                [flag, rest @ ..] if flag.starts_with('-') && flag.len() > 1 => {
                    (signal(&flag[1..]), rest)
                }
                rest => (Some(15), rest),
            };
            let number =
                number.ok_or_else(|| shell.fail("`kill` does not know that signal".into()))?;
            if targets.is_empty() {
                return Err(shell.fail("`kill` takes a job or process ID".into()));
            }
            for target in targets {
                shell
                    .system
                    .kill(target, number)
                    .map_err(|e| shell.fail(format!("{target}: {e}")))?;
            }
            Ok(Value::Null)
        }
        "exec" => {
            let mut texts: Vec<String> = values.iter().map(scalar_text).collect();
            if texts.is_empty() {
                return Err(shell.fail("`exec` takes a command".into()));
            }
            let program_name = texts.remove(0);
            let program = shell.program(&program_name, texts)?;
            let why = shell.system.exec(program);
            Err(shell.fail(why))
        }
        "history" => {
            shell.at_most(name, &values, 0)?;
            let lines: Vec<String> = shell
                .history
                .iter()
                .enumerate()
                .map(|(i, line)| format!("{:5}  {line}\n", i + 1))
                .collect();
            for line in lines {
                shell.write(Stream::Output, line.as_bytes());
            }
            Ok(Value::Null)
        }
        _ => unreachable!("not a built-in command"),
    }
}

/// A value's text for `print`: as to a program, without the last line's
/// end.
fn text(value: &Value) -> String {
    let mut text = program_text(value);
    if text.ends_with('\n') {
        text.pop();
    }
    text
}
