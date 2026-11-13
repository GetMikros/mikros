//! msh, the Mikros shell, installed as /usr/bin/msh. Without arguments
//! it runs interactively when its standard input is a terminal: it runs
//! `~/.mshrc` when that exists, then reads lines at a prompt, keeping
//! them in `~/.msh_history`. Otherwise it runs a script: the file named
//! by its first argument, the text after `-c`, or its standard input.
//! The language is that of the rest of the crate; this module gives it
//! its system and reports the errors that nothing caught.

mod os;
mod sys;
mod term;

use crate::edit::editor::{Editor, Outcome};
use crate::edit::history;
use crate::edit::keys::Decoder;
use crate::edit::render::{View, draw, finish};
use crate::eval::{Failure, Flow, Shell, Stream, System};
use crate::parse::parse;
use crate::prompt::{self, Context};
use crate::value::{Value, scalar_text};
use os::Os;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Read, Write};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use term::Terminal;

/// What msh prints before a line that continues the one before it.
const CONTINUATION: &str = "> ";

pub fn main() {
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

/// How reading a line at the prompt ended.
enum Typed {
    Line(String),
    /// Ctrl-C dropped it.
    Cancelled,
    End,
}

/// The milliseconds that an escape waits for the rest of a sequence.
const ESCAPE_WAIT: i32 = 50;

/// Writes `text` to the terminal at once.
fn show(text: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}

/// The pipe through which the thread that lists folders for suggestions
/// wakes the line editor, once that thread runs.
static WAKE: OnceLock<Wake> = OnceLock::new();

/// A pipe on which a byte means that the folders listed for suggestions
/// changed, written once until it is read.
struct Wake {
    reader: io::PipeReader,
    writer: io::PipeWriter,
    rung: AtomicBool,
}

impl Wake {
    /// Starts the thread that lists folders for the suggestions of
    /// `shell`, which wakes the line editor through the pipe.
    fn start(shell: &mut Shell<Os>) {
        let Ok((reader, writer)) = io::pipe() else {
            return;
        };
        let wake = Wake {
            reader,
            writer,
            rung: AtomicBool::new(false),
        };
        if WAKE.set(wake).is_ok() {
            shell.read_folders_in_background(Arc::new(os::Disk), || {
                if let Some(wake) = WAKE.get() {
                    wake.ring();
                }
            });
        }
    }

    fn ring(&self) {
        if !self.rung.swap(true, Ordering::AcqRel) {
            let _ = (&self.writer).write_all(b"!");
        }
    }

    /// Takes the byte that woke the line editor.
    fn answer(&self) {
        let mut byte = [0u8; 1];
        let _ = (&self.reader).read(&mut byte);
        self.rung.store(false, Ordering::Release);
    }
}

/// Reads a line with the line editor on `terminal`, after `prompt`.
fn edit_line(shell: &mut Shell<Os>, terminal: &Terminal, prompt: &str) -> Typed {
    terminal.raw();
    shell.new_line();
    let mut editor = Editor::new(shell.history.clone());
    let mut decoder = Decoder::new();
    let mut row = 0;
    // One byte at a time, so that what follows Enter stays for the
    // next line or the program that the line runs.
    let mut buf = [0u8; 1];
    let ended = loop {
        // What was typed ahead or pasted is taken in before the line is
        // drawn again: a suggestion and a frame for each of its keys
        // would make it come in slower than it was typed.
        if !term::input_within(0) {
            let (columns, rows) = term::size();
            let suggestion = editor.suggestion(&shell.without_waiting());
            let below = editor.below(columns, (rows / 2).max(1));
            let view = View {
                prompt,
                line: editor.line().text(),
                cursor: editor.line().cursor(),
                suggestion: &suggestion,
                below: &below,
            };
            let frame = draw(&view, columns, row);
            show(&frame.text);
            row = frame.cursor_row;
        }
        let keys = match decoder.waiting() && !term::input_within(ESCAPE_WAIT) {
            true => decoder.flush(),
            false => {
                // Folders listed anew can change the suggestion, which
                // is worked out and drawn again.
                if let Some(wake) = WAKE.get()
                    && term::wait_for_input_or(wake.reader.as_raw_fd()) == term::Ready::Other
                {
                    wake.answer();
                    continue;
                }
                match term::read_input(&mut buf) {
                    Ok(0) | Err(_) => break Typed::End,
                    Ok(n) => decoder.feed(&buf[..n]),
                }
            }
        };
        let mut outcome = None;
        for key in keys {
            match editor.key(key, &*shell) {
                Outcome::Continue => {}
                Outcome::Clear => {
                    show("\x1b[H\x1b[2J");
                    row = 0;
                }
                done => {
                    outcome = Some(done);
                    break;
                }
            }
        }
        match outcome {
            None => {}
            Some(Outcome::Accept(text)) => break Typed::Line(text),
            Some(Outcome::Cancel) => break Typed::Cancelled,
            Some(_) => break Typed::End,
        }
    };
    let (columns, _) = term::size();
    let text = match &ended {
        Typed::Line(text) => text.clone(),
        _ => editor.line().text().to_owned(),
    };
    show(&finish(prompt, &text, columns, row));
    terminal.restore();
    ended
}

/// Reads a statement at the prompt, over as many lines as it takes, with
/// the line editor when standard input is a terminal; an empty one when
/// Ctrl-C dropped it, and `None` at the end of input.
fn read_entry(shell: &mut Shell<Os>, terminal: Option<&Terminal>) -> Option<String> {
    let mut entry = String::new();
    let mut shown = prompt(shell);
    loop {
        let line = match terminal {
            Some(terminal) => match edit_line(shell, terminal, &shown) {
                Typed::Line(line) => line + "\n",
                Typed::Cancelled => return Some(String::new()),
                Typed::End if entry.is_empty() => return None,
                Typed::End => return Some(entry),
            },
            None => {
                shell.system.write(Stream::Output, shown.as_bytes());
                let mut line = String::new();
                match io::stdin().lock().read_line(&mut line) {
                    Ok(0) | Err(_) if entry.is_empty() => return None,
                    Ok(0) | Err(_) => return Some(entry),
                    Ok(_) => line,
                }
            }
        };
        entry.push_str(&line);
        match parse(&entry) {
            Err(e) if e.incomplete => shown = CONTINUATION.into(),
            _ => return Some(entry),
        }
    }
}

/// The most words of an entry whose paths are recorded with it.
const PATHS_MAX: usize = 16;

/// Adds `line` to the history and to the history file, with the folder
/// it was entered in and its words that name files or folders there.
fn remember(shell: &mut Shell<Os>, line: &str, file: Option<&str>) {
    let folder = shell.system.current_dir();
    let entry = history::Entry {
        line: line.into(),
        paths: relative_paths(line, &folder),
        folder: Some(folder),
    };
    let encoded = history::encode(&entry);
    shell.history.push(entry);
    let Some(file) = file else {
        return;
    };
    if let Ok(mut f) = OpenOptions::new().append(true).create(true).open(file) {
        let _ = f.write_all(encoded.as_bytes());
    }
}

/// The words of `line`, unquoted, that name a file or folder relative to
/// `folder`.
fn relative_paths(line: &str, folder: &str) -> Vec<String> {
    let base = std::path::Path::new(folder);
    line.split_whitespace()
        .map(|w| w.trim_matches(['\'', '"']))
        .filter(|w| !w.is_empty() && !w.starts_with(['/', '~', '-', '$']))
        .filter(|w| std::fs::symlink_metadata(base.join(w)).is_ok())
        .take(PATHS_MAX)
        .map(str::to_owned)
        .collect()
}

/// Runs msh at the prompt until input ends or `exit`, and gives its exit
/// status.
fn interactive() -> i64 {
    let job_control = take_terminal();
    let terminal = Terminal::new();
    let mut shell = Shell::new(Os::new(job_control), environment());
    if terminal.is_some() {
        Wake::start(&mut shell);
    }
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
        shell.history = history::decode(&text);
    }
    shell.interactive = true;
    loop {
        for line in shell.system.finished() {
            shell
                .system
                .write(Stream::Error, format!("{line}\n").as_bytes());
        }
        let Some(entry) = read_entry(&mut shell, terminal.as_ref()) else {
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
