//! tar on Mikros: it reads and writes the archive and the files through
//! the standard library, and asks the node connection of the folder that
//! receives each entry for the space of its volume with
//! `QueryFilesystem` (MIKSPEC-0098.R33, MIKSPEC-0098.R61).

use crate::args::{self, COMMAND, Mode, Select};
use crate::create::{Creator, Report};
use crate::extract::{self, Existing};
use crate::header::Entry;
use crate::inside::{Space, Target, Volume};
use crate::listing;
use crate::reader::{self, Next, Reader};
use flate2::{Compression, write::GzEncoder};
use mikros_cli::{FAILURE, INVALID, Paint, Role, Run, SUCCESS, complain, start};
use mikros_command::node;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let mode = match args::mode(&run) {
        Ok(mode) => mode,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let ok = match mode {
        Mode::Create => create(&run),
        Mode::Extract => extract(&run),
        Mode::List => list(&run),
    };
    ExitCode::from(if ok { SUCCESS } else { FAILURE })
}

/// The archive's file, none for standard input or output.
fn archive(run: &Run) -> Option<&str> {
    run.value("-f").filter(|f| *f != "-")
}

/// What a run tells: names with `-v` on standard output, or on
/// standard error when the archive goes to standard output, and
/// messages on standard error.
struct Told<'a> {
    run: &'a Run,
    verbose: bool,
    to_stderr: bool,
    paint: Paint,
    failed: bool,
}

impl<'a> Told<'a> {
    fn new(run: &'a Run, to_stderr: bool) -> Told<'a> {
        let paint = match to_stderr {
            true => Paint::stderr(run),
            false => Paint::stdout(run),
        };
        Told {
            run,
            verbose: run.has("-v"),
            to_stderr,
            paint,
            failed: false,
        }
    }
}

impl Report for Told<'_> {
    fn name(&mut self, name: &str) {
        if !self.verbose {
            return;
        }
        let line = format!("{}\n", self.paint.paint(Role::Path, name));
        let _ = match self.to_stderr {
            true => io::stderr().write_all(line.as_bytes()),
            false => io::stdout().write_all(line.as_bytes()),
        };
    }

    fn error(&mut self, message: &str) {
        complain(&COMMAND, self.run, message);
        self.failed = true;
    }

    fn note(&mut self, message: &str) {
        eprintln!("tar: {message}");
    }
}

fn create(run: &Run) -> bool {
    let to_stdout = archive(run).is_none();
    let mut told = Told::new(run, to_stdout);
    let out: Box<dyn Write> = match archive(run) {
        Some(path) => match File::create(path) {
            Ok(file) => Box::new(io::BufWriter::with_capacity(64 * 1024, file)),
            Err(e) => {
                told.error(&format!("cannot create {path}: {e}"));
                return false;
            }
        },
        None => Box::new(io::BufWriter::new(io::stdout())),
    };
    let key = archive(run)
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|m| (m.dev(), m.ino()));
    let out: Box<dyn Write> = match run.has("-z") {
        true => Box::new(GzEncoder::new(out, Compression::default())),
        false => out,
    };
    let mut select = Select::new(&[], &run.values("--exclude"));
    let from = run.value("-C").map(PathBuf::from);
    let mut creator = Creator::new(out, &mut select, &mut told, key);
    let mut written = Ok(());
    for operand in &run.operands {
        let path = match &from {
            Some(from) => from.join(operand),
            None => PathBuf::from(operand),
        };
        written = creator.add(&path, operand);
        if written.is_err() {
            break;
        }
    }
    let written = written.and_then(|()| creator.finish().map(drop));
    if let Err(e) = written {
        told.error(&format!("cannot write the archive: {e}"));
    }
    !told.failed
}

/// The archive to read, gunzipped when it is gzip.
fn input(run: &Run, told: &mut Told) -> Option<Box<dyn Read>> {
    let raw: Box<dyn Read> = match archive(run) {
        Some(path) => match File::open(path) {
            Ok(file) => Box::new(file),
            Err(e) => {
                told.error(&format!("cannot open {path}: {e}"));
                return None;
            }
        },
        None => Box::new(io::stdin()),
    };
    match reader::unpacked(raw) {
        Ok(input) => Some(input),
        Err(e) => {
            told.error(&format!("cannot read the archive: {e}"));
            None
        }
    }
}

fn extract(run: &Run) -> bool {
    let mut told = Told::new(run, false);
    let Some(input) = input(run, &mut told) else {
        return false;
    };
    let root = run.value("-C").unwrap_or(".");
    let target = match Target::new(Path::new(root)) {
        Ok(target) => target,
        Err(e) => {
            told.error(&format!("cannot extract into {root}: {e}"));
            return false;
        }
    };
    let mut select = Select::new(&run.operands, &run.values("--exclude"));
    let existing = match run.has("-k") {
        true => Existing::Keep,
        false => Existing::Replace,
    };
    let mut reader = Reader::new(input, true);
    let ok = extract::extract(
        &mut reader,
        &target,
        &mut Volumes,
        &mut select,
        existing,
        &mut told,
    );
    ok && !told.failed
}

fn list(run: &Run) -> bool {
    let mut told = Told::new(run, false);
    let Some(input) = input(run, &mut told) else {
        return false;
    };
    let mut select = Select::new(&run.operands, &run.values("--exclude"));
    let mut reader = Reader::new(input, false);
    let mut entries: Vec<Entry> = Vec::new();
    let mut broken = None;
    loop {
        match reader.next() {
            Ok(Next::End) => break,
            Ok(Next::Entry(entry)) if select.takes(&entry.path) => entries.push(entry),
            Ok(_) => {}
            Err(why) => {
                broken = Some(why);
                break;
            }
        }
    }
    let text = match (run.has("--json"), run.has("-v")) {
        (true, _) => listing::json(&entries),
        (false, true) => listing::long(&entries, Paint::stdout(run)),
        (false, false) => listing::names(&entries, Paint::stdout(run)),
    };
    if let Err(e) = io::stdout().write_all(text.as_bytes()) {
        told.error(&format!("cannot write the list: {e}"));
    }
    if let Some(why) = broken {
        told.error(&why);
    }
    for name in select.missing() {
        told.error(&format!("{name}: not found in the archive"));
    }
    !told.failed
}

/// The volumes, as `QueryFilesystem` gives their space.
struct Volumes;

impl Space for Volumes {
    fn volume(&mut self, folder: &Path) -> Result<Volume, String> {
        let shown = folder.display();
        let node = node::open(folder, true).map_err(|e| format!("cannot open {shown}: {e}"))?;
        let info = node::filesystem(&node)
            .map_err(|e| format!("cannot ask the volume of {shown} for its space: {e:?}"))?;
        let name = [&info.label, &info.id, &info.r#type]
            .into_iter()
            .find(|n| !n.is_empty())
            .cloned()
            .unwrap_or_else(|| shown.to_string());
        Ok(Volume {
            name,
            total: info.total_bytes,
            free: info.free_bytes,
        })
    }
}
