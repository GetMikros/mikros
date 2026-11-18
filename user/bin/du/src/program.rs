//! du on Mikros: it walks the folders through the standard library,
//! without following links, and counts each inode once.

use crate::space::{self, COMMAND, Show};
use mikros_cli::{FAILURE, INVALID, Paint, Run, SUCCESS, complain, start};
use std::collections::HashSet;
use std::fs;
use std::io::{BufWriter, Stdout, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::ExitCode;

/// What the run asks and what it has seen.
struct Walker<'a> {
    run: &'a Run,
    show: Show,
    paint: Paint,
    /// The device and inode of every entry counted.
    seen: HashSet<(u64, u64)>,
    failed: bool,
    out: BufWriter<Stdout>,
    /// Every path shown, for JSON.
    all: Vec<(String, u64)>,
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let show = match Show::of(&run) {
        Ok(show) => show,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let paint = match show.json {
        true => Paint::never(),
        false => Paint::stdout(&run),
    };
    let mut walker = Walker {
        run: &run,
        show,
        paint,
        seen: HashSet::new(),
        failed: false,
        out: BufWriter::new(std::io::stdout()),
        all: Vec::new(),
    };
    let operands: Vec<String> = match run.operands.is_empty() {
        true => vec![".".into()],
        false => run.operands.clone(),
    };
    let mut total = 0u64;
    for operand in &operands {
        total = total.saturating_add(walker.measure(Path::new(operand), 0));
    }
    if show.total {
        walker.emit("total", total);
    }
    let mut written = Ok(());
    if show.json {
        let text = space::json(&walker.all);
        written = walker.out.write_all(text.as_bytes());
    }
    let written = written.and_then(|()| walker.out.flush());
    match walker.failed || written.is_err() {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

impl Walker<'_> {
    /// The bytes that `path`, `depth` below its operand, and the entries
    /// below it use, after showing those that the run asks for; nothing
    /// for an inode counted before.
    fn measure(&mut self, path: &Path, depth: usize) -> u64 {
        let metadata = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) => {
                self.fail(&format!("cannot access {}: {e}", path.display()));
                return 0;
            }
        };
        if !self.seen.insert((metadata.dev(), metadata.ino())) {
            return 0;
        }
        let mut bytes = match self.show.apparent {
            true => metadata.size(),
            false => metadata.blocks().saturating_mul(512),
        };
        let folder = metadata.is_dir();
        if folder {
            for name in self.entries(path) {
                let below = self.measure(&path.join(name), depth + 1);
                bytes = bytes.saturating_add(below);
            }
        }
        if self.show.shows(depth, folder) {
            self.emit(&path.display().to_string(), bytes);
        }
        bytes
    }

    /// The names in `folder`, in order.
    fn entries(&mut self, folder: &Path) -> Vec<String> {
        let listing = match fs::read_dir(folder) {
            Ok(listing) => listing,
            Err(e) => {
                self.fail(&format!("cannot read folder {}: {e}", folder.display()));
                return Vec::new();
            }
        };
        let mut names = Vec::new();
        for item in listing {
            match item {
                Ok(item) => names.push(item.file_name().to_string_lossy().into_owned()),
                Err(e) => self.fail(&format!("reading {}: {e}", folder.display())),
            }
        }
        names.sort();
        names
    }

    fn emit(&mut self, path: &str, bytes: u64) {
        match self.show.json {
            true => self.all.push((path.into(), bytes)),
            false => {
                let line = space::line(bytes, path, self.show.unit, self.paint);
                if self.out.write_all(line.as_bytes()).is_err() {
                    self.failed = true;
                }
            }
        }
    }

    fn fail(&mut self, message: &str) {
        // What came before the failure is shown first.
        let _ = self.out.flush();
        complain(&COMMAND, self.run, message);
        self.failed = true;
    }
}
