//! unzip on Mikros: it reads the archive and writes the files through
//! the standard library, and asks the node connection of the folder that
//! receives the entries for the space of its volume with
//! `QueryFilesystem` (MIKSPEC-0098.R33, MIKSPEC-0098.R61).

use crate::args::{self, COMMAND, Mode};
use crate::central::{self, Record};
use crate::data::{self, Crc};
use crate::extract::{self, Options, Report};
use crate::inside::{Space, Volume};
use crate::listing;
use mikros_cli::{FAILURE, INVALID, Paint, Role, Run, SUCCESS, complain, start};
use mikros_command::node;
use std::fs::File;
use std::io::{self, BufReader, IsTerminal, Write};
use std::path::Path;
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
    let mut told = Told {
        run: &run,
        paint: Paint::stdout(&run),
        failed: false,
    };
    let name = &run.operands[0];
    let mut file = match File::open(name) {
        Ok(file) => BufReader::with_capacity(64 * 1024, file),
        Err(e) => {
            told.error(&format!("cannot open {name}: {e}"));
            return ExitCode::from(FAILURE);
        }
    };
    let directory = match central::read(&mut file) {
        Ok(directory) => directory,
        Err(why) => {
            told.error(&format!("{name}: {why}"));
            return ExitCode::from(FAILURE);
        }
    };
    let (chosen, unmatched) =
        args::chosen(&directory.records, &run.operands[1..], &run.values("-x"));
    let records: Vec<&Record> = chosen.iter().map(|&i| &directory.records[i]).collect();
    match mode {
        Mode::List => {
            let text = match run.has("--json") {
                true => listing::json(&records),
                false => listing::list(&records, told.paint),
            };
            if let Err(e) = io::stdout().write_all(text.as_bytes()) {
                told.error(&format!("cannot write the list: {e}"));
            }
        }
        Mode::Test => test(&mut file, &records, &mut told, name),
        Mode::Pipe => pipe(&mut file, &records, &mut told),
        Mode::Extract => {
            let root = Path::new(run.value("-d").unwrap_or("."));
            let options = Options {
                junk: run.has("-j"),
                existing: args::existing(&run),
            };
            if !extract::extract(
                &mut file,
                &directory,
                &chosen,
                root,
                &mut Volumes,
                options,
                &mut told,
            ) {
                told.failed = true;
            }
        }
    }
    for name in unmatched {
        told.error(&format!("{name}: not found in the archive"));
    }
    ExitCode::from(if told.failed { FAILURE } else { SUCCESS })
}

/// Checks the CRC-32 and size of every entry's data.
fn test(file: &mut BufReader<File>, records: &[&Record], told: &mut Told, name: &str) {
    let quiet = told.run.has("-q");
    for record in records.iter().filter(|r| !r.is_folder()) {
        let checked = data::open(file, record).and_then(|data| {
            let mut data = Crc::new(data);
            io::copy(&mut data, &mut io::sink())
                .map_err(|e| format!("cannot read its data: {e}"))?;
            if data.count != record.size {
                return Err(format!(
                    "its data is {} bytes, not the {} that the archive gives",
                    data.count, record.size
                ));
            }
            data::checked(record, data.crc())
        });
        match checked {
            Ok(()) if !quiet => told.did("testing", &format!("{}  OK", record.name)),
            Ok(()) => {}
            Err(why) => told.error(&format!("{}: {why}", record.name)),
        }
    }
    if !told.failed && !quiet {
        println!("no errors in {}", told.paint.paint(Role::Path, name));
    }
}

/// Writes the contents of the entries to standard output.
fn pipe(file: &mut BufReader<File>, records: &[&Record], told: &mut Told) {
    let mut out = io::stdout().lock();
    for record in records.iter().filter(|r| !r.is_folder()) {
        let written = data::open(file, record).and_then(|data| {
            let mut data = Crc::new(data);
            io::copy(&mut data, &mut out).map_err(|e| format!("cannot write its data: {e}"))?;
            data::checked(record, data.crc())
        });
        if let Err(why) = written {
            told.error(&format!("{}: {why}", record.name));
        }
    }
    let _ = out.flush();
}

/// What a run tells: what it did with each entry on standard output,
/// unless `-q`, and errors and messages on standard error.
struct Told<'a> {
    run: &'a Run,
    paint: Paint,
    failed: bool,
}

impl Report for Told<'_> {
    fn did(&mut self, what: &str, name: &str) {
        if !self.run.has("-q") {
            let line = format!("{what:>10}: {}\n", self.paint.paint(Role::Path, name));
            let _ = io::stdout().write_all(line.as_bytes());
        }
    }

    fn error(&mut self, message: &str) {
        complain(&COMMAND, self.run, message);
        self.failed = true;
    }

    fn note(&mut self, message: &str) {
        eprintln!("unzip: {message}");
    }

    fn replace(&mut self, name: &str) -> Option<bool> {
        if !io::stdin().is_terminal() {
            return None;
        }
        eprint!("unzip: replace {name}? [y/n] ");
        let mut answer = String::new();
        let read = io::stdin().read_line(&mut answer);
        Some(read.is_ok() && answer.trim_start().starts_with(['y', 'Y']))
    }
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
