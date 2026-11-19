//! zip on Mikros: it reads the files and the archive that is there and
//! writes the new archive through the standard library, into a file
//! beside the old one that then takes its place (MIKSPEC-0098.R33).

use crate::args::{self, COMMAND};
use crate::central::{self, Directory};
use crate::pattern::Pattern;
use crate::update::{self, Options, Report};
use mikros_cli::{FAILURE, Paint, Role, Run, SUCCESS, complain, start};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// What a run tells: what it did with each entry on standard output,
/// unless `-q`, and errors and messages on standard error.
struct Told<'a> {
    run: &'a Run,
    paint: Paint,
    failed: bool,
}

impl Report for Told<'_> {
    fn did(&mut self, what: &str, name: &str, how: &str) {
        if !self.run.has("-q") {
            let name = self.paint.entry(name);
            let line = match how.is_empty() {
                true => format!("{what:>8}: {name}\n"),
                false => format!(
                    "{what:>8}: {name} {}\n",
                    self.paint.paint(Role::Secondary, how)
                ),
            };
            let _ = io::stdout().write_all(line.as_bytes());
        }
    }

    fn error(&mut self, message: &str) {
        complain(&COMMAND, self.run, message);
        self.failed = true;
    }

    fn note(&mut self, message: &str) {
        eprintln!("zip: {message}");
    }
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let mut told = Told {
        run: &run,
        paint: Paint::stdout(&run),
        failed: false,
    };
    let archive = Path::new(&run.operands[0]);
    let paths = &run.operands[1..];
    let mut old: Option<(BufReader<File>, Directory)> = None;
    if archive.exists() {
        let opened =
            File::open(archive).map_err(|e| format!("cannot open {}: {e}", archive.display()));
        let read = opened.and_then(|file| {
            let mut file = BufReader::new(file);
            let directory =
                central::read(&mut file).map_err(|why| format!("{}: {why}", archive.display()))?;
            Ok((file, directory))
        });
        match read {
            Ok(read) => old = Some(read),
            Err(why) => {
                told.error(&why);
                return ExitCode::from(FAILURE);
            }
        }
    } else if run.has("-d") {
        told.error(&format!(
            "cannot open {}: there is no such archive",
            archive.display()
        ));
        return ExitCode::from(FAILURE);
    }
    let members = match run.has("-d") {
        true => Vec::new(),
        false => {
            let options = Options {
                recurse: run.has("-r"),
                junk: run.has("-j"),
                links: run.has("-y"),
                exclude: run.values("-x").iter().map(|p| Pattern::new(p)).collect(),
            };
            let key = fs::metadata(archive).ok().map(|m| (m.dev(), m.ino()));
            let members = update::gather(paths, &options, key, &mut told);
            if members.is_empty() && old.is_none() {
                told.error("nothing to store");
                return ExitCode::from(FAILURE);
            }
            members
        }
    };
    let temporary = beside(archive);
    let made = OpenOptions::new()
        .write(true)
        .read(true)
        .create_new(true)
        .open(&temporary);
    let file = match made {
        Ok(file) => file,
        Err(e) => {
            told.error(&format!("cannot create {}: {e}", temporary.display()));
            return ExitCode::from(FAILURE);
        }
    };
    let out = BufWriter::with_capacity(64 * 1024, file);
    let written = match (&mut old, run.has("-d")) {
        (Some((from, directory)), true) => update::delete(out, from, directory, paths, &mut told),
        (Some((from, directory)), false) => update::update(
            out,
            Some((from, &*directory)),
            &members,
            args::level(&run),
            &mut told,
        ),
        (None, _) => {
            update::update::<_, BufReader<File>>(out, None, &members, args::level(&run), &mut told)
        }
    };
    let placed = written
        .and_then(|(out, length)| {
            let file = out.into_inner().map_err(|e| e.into_error())?;
            file.set_len(length)?;
            file.sync_all()
        })
        .and_then(|()| {
            if let Ok(metadata) = fs::metadata(archive) {
                fs::set_permissions(&temporary, metadata.permissions())?;
            }
            fs::rename(&temporary, archive)
        });
    if let Err(e) = placed {
        let _ = fs::remove_file(&temporary);
        told.error(&format!("cannot write {}: {e}", archive.display()));
    }
    ExitCode::from(if told.failed { FAILURE } else { SUCCESS })
}

/// A path for the new archive beside `archive`.
fn beside(archive: &Path) -> PathBuf {
    let mut name = archive.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".zip-{}", std::process::id()));
    archive.with_file_name(name)
}
