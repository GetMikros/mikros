//! msh, the Mikros shell, installed as /usr/bin/msh. Without arguments
//! it runs interactively when its standard input is a terminal: it runs
//! `~/.mshrc` when that exists, then reads lines at a prompt, keeping
//! them in `~/.msh_history`. Otherwise it runs a script: the file named
//! by its first argument, the text after `-c`, or its standard input.
//! The language is that of the `mikros-msh` library; this program gives
//! the library its system and reports the errors that nothing caught.

mod os;
mod sys;

use mikros_msh::eval::{Failure, Flow, Shell, Stream, System};
use mikros_msh::parse::parse;
use mikros_msh::prompt::{self, Context};
use mikros_msh::value::{Value, scalar_text};
use os::Os;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Read, Write};

/// What msh prints before a line that continues the one before it.
const CONTINUATION: &str = "> ";

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
        [] if sys::is_terminal(0) => interactive(),
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

/// The home folder, from `$env.HOME`.
fn home(shell: &Shell<Os>) -> Option<String> {
    shell.env("HOME").map(scalar_text).filter(|h| !h.is_empty())
}

/// Puts the shell in a process group of its own in the foreground of its
/// controlling terminal, and tells whether it then can run jobs, which
/// it cannot without a controlling terminal.
fn take_terminal() -> bool {
    sys::shield();
    if sys::foreground().is_none() {
        return false;
    }
    // A session leader leads its group already.
    let _ = sys::set_group(0, 0);
    sys::set_foreground(sys::group()).is_ok()
}

/// The prompt, from `$env.PROMPT` or the default.
fn prompt(shell: &Shell<Os>) -> String {
    let template = shell
        .env("PROMPT")
        .map_or_else(|| prompt::DEFAULT.to_owned(), scalar_text);
    let user = shell.env("USER").map(scalar_text).unwrap_or_default();
    let folder = shell.system.current_dir();
    let home = home(shell);
    let cx = Context {
        user: &user,
        host: &sys::host_name(),
        folder: &folder,
        home: home.as_deref(),
        elevated: false,
        status: shell.status(),
        local_time: sys::local_time(),
    };
    prompt::expand(&template, &cx)
}

/// Reads a statement at the prompt, over as many lines as it takes, or
/// `None` at the end of input.
fn read_entry(shell: &mut Shell<Os>) -> Option<String> {
    let mut entry = String::new();
    let mut shown = prompt(shell);
    loop {
        shell.system.write(Stream::Output, shown.as_bytes());
        let mut line = String::new();
        match io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) if entry.is_empty() => return None,
            Ok(0) | Err(_) => return Some(entry),
            Ok(_) => {}
        }
        entry.push_str(&line);
        match parse(&entry) {
            Err(e) if e.incomplete => shown = CONTINUATION.into(),
            _ => return Some(entry),
        }
    }
}

/// Adds `entry` to the history and to the history file.
fn remember(shell: &mut Shell<Os>, entry: &str, file: Option<&str>) {
    shell.history.push(entry.into());
    let Some(file) = file else {
        return;
    };
    if let Ok(mut f) = OpenOptions::new().append(true).create(true).open(file) {
        let _ = writeln!(f, "{entry}");
    }
}

/// Runs msh at the prompt until input ends or `exit`, and gives its exit
/// status.
fn interactive() -> i64 {
    let job_control = take_terminal();
    let mut shell = Shell::new(Os::new(job_control), environment());
    let home = home(&shell);
    if let Some(rc) = home.as_ref().map(|h| format!("{h}/.mshrc"))
        && let Ok(text) = std::fs::read_to_string(&rc)
    {
        shell.script = Some(rc);
        if let Some(status) = run_script(&mut shell, &text, &[]) {
            shell.set_global("status", Value::Int(status));
        }
        shell.script = None;
    }
    let history = home.as_ref().map(|h| format!("{h}/.msh_history"));
    if let Some(text) = history
        .as_ref()
        .and_then(|f| std::fs::read_to_string(f).ok())
    {
        shell.history = text.lines().map(Into::into).collect();
    }
    shell.interactive = true;
    loop {
        for line in shell.system.finished() {
            shell
                .system
                .write(Stream::Error, format!("{line}\n").as_bytes());
        }
        let Some(entry) = read_entry(&mut shell) else {
            return shell.status();
        };
        let entry = entry.trim_end_matches('\n');
        if entry.trim().is_empty() {
            continue;
        }
        remember(&mut shell, entry, history.as_deref());
        let block = match parse(entry) {
            Ok(block) => block,
            Err(e) => {
                eprintln!("msh: {}", e.message);
                shell.set_global("status", Value::Int(1));
                continue;
            }
        };
        match shell.top(&block) {
            Ok(()) | Err(Flow::Return(_)) => {}
            Err(Flow::Exit(status)) => return status,
            Err(Flow::Error(failure)) => {
                report(&mut shell, &failure);
                shell.set_global("status", Value::Int(failure.status));
            }
        }
    }
}
