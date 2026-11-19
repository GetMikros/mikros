//! grep on Mikros: it reads the patterns of `-f`, the files and standard
//! input through the standard library, walks folders with `-r` and `-R`,
//! and exits with 0, 1 or 2 as MIKSPEC-0098.R52 asks.

use crate::pattern::Matcher;
use crate::search::{self, COMMAND, Failed, Input, Options, glob};
use mikros_cli::{INVALID, Paint, Run, complain, start};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::process::ExitCode;

/// The exit status when a line was selected.
const SELECTED: u8 = 0;
/// The exit status when none was.
const NONE: u8 = 1;
/// The exit status on an error.
const ERROR: u8 = 2;

/// The name of standard input in text.
const STDIN: &str = "(standard input)";

struct Grep<'a, W: Write> {
    run: &'a Run,
    matcher: Matcher,
    options: Options,
    paint: Paint,
    out: W,
    json: Vec<String>,
    selected: bool,
    failed: bool,
    recursive: bool,
    follow: bool,
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let invalid = |message: &str| -> ExitCode {
        complain(&COMMAND, &run, message);
        ExitCode::from(INVALID)
    };
    let mut options = match Options::of(&run) {
        Ok(options) => options,
        Err(why) => return invalid(&why),
    };
    let mut operands = run.operands.clone();
    let mut patterns: Vec<String> = Vec::new();
    for pattern in run.values("-e") {
        patterns.extend(pattern.split('\n').map(String::from));
    }
    for file in run.values("-f") {
        match read_patterns(file) {
            Ok(read) => patterns.extend(read),
            Err(e) => {
                complain(&COMMAND, &run, &format!("{file}: {e}"));
                return ExitCode::from(ERROR);
            }
        }
    }
    if !run.has("-e") && !run.has("-f") {
        if operands.is_empty() {
            return invalid("missing operand <pattern>");
        }
        patterns.extend(operands.remove(0).split('\n').map(String::from));
    }
    let matcher = match Matcher::new(&patterns, search::settings(&run)) {
        Ok(matcher) => matcher,
        Err(why) => return invalid(&why),
    };
    let recursive = run.has("-r") || run.has("-R");
    let implicit = operands.is_empty();
    if implicit {
        operands.push(if recursive { ".".into() } else { "-".into() });
    }
    options.with_name = if run.has("-H") {
        true
    } else if run.has("-h") {
        false
    } else {
        operands.len() > 1
            || (recursive && operands.iter().any(|o| o != "-" && Path::new(o).is_dir()))
    };
    let paint = match options.json {
        true => Paint::never(),
        false => Paint::stdout(&run),
    };
    let mut grep = Grep {
        run: &run,
        matcher,
        options,
        paint,
        out: BufWriter::new(io::stdout().lock()),
        json: Vec::new(),
        selected: false,
        failed: false,
        recursive,
        follow: run.has("-R"),
    };
    let written = grep.operands(&operands, implicit);
    let written = written.and_then(|()| {
        if grep.options.json {
            let text = format!("[{}]\n", grep.json.join(","));
            grep.out.write_all(text.as_bytes())?;
        }
        grep.out.flush()
    });
    if let Err(e) = written
        && e.kind() != io::ErrorKind::BrokenPipe
    {
        complain(&COMMAND, &run, &format!("standard output: {e}"));
        grep.failed = true;
    }
    ExitCode::from(grep.status())
}

/// The patterns of the file `name`, one per line, or of standard input
/// for `-`.
fn read_patterns(name: &str) -> io::Result<Vec<String>> {
    let mut bytes = Vec::new();
    match name {
        "-" => io::stdin().lock().read_to_end(&mut bytes)?,
        path => File::open(path)?.read_to_end(&mut bytes)?,
    };
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    match text.is_empty() && bytes.is_empty() {
        true => Ok(Vec::new()),
        false => Ok(text.split('\n').map(String::from).collect()),
    }
}

impl<W: Write> Grep<'_, W> {
    fn status(&self) -> u8 {
        match (self.failed, self.selected) {
            (_, true) if self.options.quiet => SELECTED,
            (true, _) => ERROR,
            (false, true) => SELECTED,
            (false, false) => NONE,
        }
    }

    /// Searches the operands; with `implicit`, `.` is searched with
    /// the names below it shown without `./`.
    fn operands(&mut self, operands: &[String], implicit: bool) -> io::Result<()> {
        for operand in operands {
            if operand == "-" {
                let mut stdin = io::stdin().lock();
                self.input(&mut stdin, STDIN, "-")?;
                continue;
            }
            let path = Path::new(operand);
            if self.recursive && path.is_dir() {
                let prefix = match implicit {
                    true => String::new(),
                    false => match operand.ends_with('/') {
                        true => operand.clone(),
                        false => format!("{operand}/"),
                    },
                };
                self.folder(path, &prefix)?;
            } else if self.wanted(operand) {
                self.file(path, operand)?;
            }
        }
        Ok(())
    }

    /// Whether `--include` and `--exclude` let the file at `path` be
    /// searched, by its name.
    fn wanted(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        let includes = self.run.values("--include");
        let included = includes.is_empty() || includes.iter().any(|g| glob(g, name));
        included && !self.run.values("--exclude").iter().any(|g| glob(g, name))
    }

    /// Searches the files below `folder`, in the order of their names,
    /// naming them after `prefix`.
    fn folder(&mut self, folder: &Path, prefix: &str) -> io::Result<()> {
        let entries = match fs::read_dir(folder) {
            Ok(entries) => entries,
            Err(e) => {
                self.unreadable(&format!("{}: {e}", folder.display()));
                return Ok(());
            }
        };
        let mut names: Vec<String> = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => names.push(entry.file_name().to_string_lossy().into_owned()),
                Err(e) => self.unreadable(&format!("{}: {e}", folder.display())),
            }
        }
        names.sort();
        for name in names {
            let path = folder.join(&name);
            let shown = format!("{prefix}{name}");
            let metadata = match fs::symlink_metadata(&path) {
                Ok(m) if m.file_type().is_symlink() && !self.follow => continue,
                Ok(m) if m.file_type().is_symlink() => fs::metadata(&path),
                other => other,
            };
            match metadata {
                Ok(m) if m.is_dir() => self.folder(&path, &format!("{shown}/"))?,
                Ok(_) if self.wanted(&shown) => self.file(&path, &shown)?,
                Ok(_) => {}
                Err(e) => self.unreadable(&format!("{shown}: {e}")),
            }
        }
        Ok(())
    }

    fn file(&mut self, path: &Path, shown: &str) -> io::Result<()> {
        match File::open(path) {
            Ok(file) => {
                let mut reader = BufReader::with_capacity(64 * 1024, file);
                self.input(&mut reader, shown, shown)
            }
            Err(e) => {
                self.unreadable(&format!("{shown}: {e}"));
                Ok(())
            }
        }
    }

    /// Searches one input, and writes its count or name where asked.
    /// Fails only when writing fails.
    fn input(&mut self, reader: &mut impl io::BufRead, name: &str, path: &str) -> io::Result<()> {
        let input = Input { name, path };
        let found = search::search(
            reader,
            &input,
            &self.matcher,
            &self.options,
            self.paint,
            &mut self.out,
            &mut self.json,
        );
        let selected = match found {
            Ok(selected) => selected,
            Err(Failed::Write(e)) => return Err(e),
            Err(Failed::Read(e)) => {
                self.unreadable(&format!("{name}: {e}"));
                return Ok(());
            }
        };
        if selected > 0 {
            self.selected = true;
            if self.options.quiet {
                // Nothing is written, and nothing more is needed.
                let _ = self.out.flush();
                std::process::exit(i32::from(SELECTED));
            }
        }
        let o = &self.options;
        let listed = (o.files_with && selected > 0) || (o.files_without && selected == 0);
        if listed {
            let name = self.paint.entry(name);
            writeln!(self.out, "{name}")?;
        } else if o.count && !o.files_with && !o.files_without {
            let line = search::count_line(name, selected, o, self.paint);
            self.out.write_all(line.as_bytes())?;
        }
        Ok(())
    }

    /// Reports a file or folder that could not be read, unless `-s`.
    fn unreadable(&mut self, message: &str) {
        self.failed = true;
        if !self.run.has("-s") {
            let _ = self.out.flush();
            complain(&COMMAND, self.run, message);
        }
    }
}
