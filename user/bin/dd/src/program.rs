//! dd on Mikros: it opens its files through the standard library, seeks
//! in them for `skip=` and `seek=` where it can, and copies.

use crate::transfer::{self, COMMAND, Failure, Input, Settings, Status, Tally};
use mikros_cli::{FAILURE, INVALID, SUCCESS, complain, start};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Stdin, Stdout, Write};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// How often `status=progress` shows the transfer.
const PROGRESS: Duration = Duration::from_secs(1);

/// Where blocks come from.
enum Source {
    File(File),
    Stdin(Stdin),
}

impl Read for Source {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Source::File(f) => f.read(buf),
            Source::Stdin(s) => s.read(buf),
        }
    }
}

impl Input for Source {
    fn pass(&mut self, bytes: u64) -> bool {
        let Ok(bytes) = i64::try_from(bytes) else {
            return false;
        };
        match self {
            Source::File(f) => f.seek(SeekFrom::Current(bytes)).is_ok(),
            Source::Stdin(_) => false,
        }
    }
}

/// Where blocks go.
enum Sink {
    File(File),
    Stdout(Stdout),
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Sink::File(f) => f.write(buf),
            Sink::Stdout(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Sink::File(f) => f.flush(),
            Sink::Stdout(s) => s.flush(),
        }
    }
}

impl Sink {
    /// Has the file system write the output to its storage.
    fn sync(&mut self) -> io::Result<()> {
        match self {
            Sink::File(f) => f.sync_all(),
            Sink::Stdout(s) => {
                s.flush()?;
                // SAFETY: descriptor 1 stays open; the file is never
                // dropped, so it never closes it.
                let file = ManuallyDrop::new(unsafe { File::from_raw_fd(1) });
                file.sync_all()
            }
        }
    }
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let settings = match Settings::parse(&run.operands) {
        Ok(settings) => settings,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let fail = |message: String| {
        complain(&COMMAND, &run, &message);
        ExitCode::from(FAILURE)
    };
    let input_name = settings.input.as_deref().unwrap_or("standard input");
    let output_name = settings.output.as_deref().unwrap_or("standard output");
    let started = Instant::now();
    let mut input = match &settings.input {
        Some(path) => match File::open(path) {
            Ok(file) => Source::File(file),
            Err(e) => return fail(format!("cannot open {path}: {e}")),
        },
        None => Source::Stdin(io::stdin()),
    };
    let mut output = match &settings.output {
        Some(path) => match open_output(path, &settings) {
            Ok(file) => Sink::File(file),
            Err(e) => return fail(format!("cannot open {path}: {e}")),
        },
        None => Sink::Stdout(io::stdout()),
    };
    let skipped = match &mut input {
        Source::File(file) => file.seek(SeekFrom::Start(settings.skipped())).map(drop),
        Source::Stdin(stdin) => transfer::discard(stdin, settings.skipped()).map(drop),
    };
    if let Err(e) = skipped {
        return fail(format!("cannot skip in {input_name}: {e}"));
    }
    let sought = match &mut output {
        Sink::File(file) => file.seek(SeekFrom::Start(settings.sought())).map(drop),
        Sink::Stdout(stdout) => transfer::zeros(stdout, settings.sought()),
    };
    if let Err(e) = sought {
        return fail(format!("cannot seek in {output_name}: {e}"));
    }
    let mut failed = false;
    let mut shown = None::<Instant>;
    let mut tally = Tally::default();
    let copied = transfer::copy(
        &settings,
        &mut input,
        &mut output,
        &mut tally,
        &mut |e| {
            complain(&COMMAND, &run, &format!("error reading {input_name}: {e}"));
            failed = true;
        },
        &mut |tally| {
            if settings.status == Status::Progress
                && shown.is_none_or(|at| at.elapsed() >= PROGRESS)
            {
                shown = Some(Instant::now());
                let line = transfer::transfer(tally.bytes, started.elapsed());
                eprint!("\r{line}");
            }
        },
    );
    if shown.is_some() {
        eprintln!();
    }
    match copied {
        Ok(()) => {}
        Err(Failure::Read(e)) => {
            complain(&COMMAND, &run, &format!("error reading {input_name}: {e}"));
            failed = true;
        }
        Err(Failure::Write(e)) => {
            complain(&COMMAND, &run, &format!("error writing {output_name}: {e}"));
            failed = true;
        }
    }
    if settings.fsync
        && let Err(e) = output.sync()
    {
        complain(&COMMAND, &run, &format!("cannot sync {output_name}: {e}"));
        failed = true;
    }
    eprint!(
        "{}",
        transfer::report(&tally, started.elapsed(), settings.status)
    );
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

/// The output file `path`, made when it is missing, and cut at the
/// output that `seek=` passes unless `conv=notrunc`.
fn open_output(path: &str, settings: &Settings) -> io::Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    if !settings.notrunc {
        file.set_len(settings.sought())?;
    }
    Ok(file)
}
