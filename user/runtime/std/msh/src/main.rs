//! msh, the Mikros shell, installed as /usr/bin/msh. It runs a script:
//! the file named by its first argument, the text after `-c`, or, without
//! arguments, its standard input. The language is that of the
//! `mikros-msh` library; this program gives the library its system and
//! reports the errors that nothing caught.
mod os;
mod sys;

use mikros_msh::eval::{Failure, Flow, Shell, Stream, System};
use mikros_msh::parse::parse;
use os::Os;
use std::io::{self, Read};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(start(&args) as i32);
}

fn environment() -> Vec<(String, String)> {
    std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().into(), v.to_string_lossy().into()))
        .collect()
}

/// Runs msh as `args` say, and gives its exit status.
fn start(args: &[String]) -> i64 {
    match args {
        [] => {
            let mut text = String::new();
            if let Err(e) = io::stdin().read_to_string(&mut text) {
                eprintln!("msh: standard input: {}", os::reason(&e));
                return 1;
            }
            script("stdin", &text, &[])
        }
        [flag] if flag == "-c" => {
            eprintln!("msh: -c takes the commands to run");
            1
        }
        [flag, text, rest @ ..] if flag == "-c" => script("-c", text, rest),
        [path, rest @ ..] => match std::fs::read_to_string(path) {
            Ok(text) => script(path, &text, rest),
            Err(e) => {
                eprintln!("msh: {path}: {}", os::reason(&e));
                1
            }
        },
    }
}

/// Writes `failure` to standard error, with the script's name and the
/// line unless it happened at the prompt. An error about the script's
/// options, before it runs, has no line.
fn report(shell: &mut Shell<Os>, failure: &Failure) {
    let mut line = String::from("msh: ");
    match (&shell.script, failure.line) {
        (Some(script), 0) => line.push_str(&format!("{script}: ")),
        (Some(script), at) => line.push_str(&format!("{script}:{at}: ")),
        (None, _) => {}
    }
    if !failure.command.is_empty() {
        line.push_str(&format!("{}: ", failure.command));
    }
    line.push_str(&failure.message);
    line.push('\n');
    shell.system.write(Stream::Error, line.as_bytes());
}

/// Runs the script `text`, called `name`, with `args` as its options and
/// `$args`, and gives the status msh exits with.
fn script(name: &str, text: &str, args: &[String]) -> i64 {
    let mut shell = Shell::new(Os::new(false), environment());
    shell.script = Some(name.into());
    run_script(&mut shell, text, args).unwrap_or_else(|| shell.status())
}

/// Runs `text` in `shell`, and gives the status of the error or `exit`
/// that ended it early.
fn run_script(shell: &mut Shell<Os>, text: &str, args: &[String]) -> Option<i64> {
    let block = match parse(text) {
        Ok(block) => block,
        Err(e) => {
            let name = shell.script.clone().unwrap_or_default();
            eprintln!("msh: {name}:{}: {}", e.line, e.message);
            return Some(1);
        }
    };
    let ran = shell.options(&block, args).and_then(|()| shell.top(&block));
    match ran {
        Ok(()) | Err(Flow::Return(_)) => None,
        Err(Flow::Exit(status)) => Some(status),
        Err(Flow::Error(failure)) => {
            report(shell, &failure);
            Some(failure.status)
        }
    }
}
