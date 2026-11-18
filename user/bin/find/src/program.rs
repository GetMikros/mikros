//! find on Mikros: it walks the folders through the standard library,
//! not following links unless `-L` is given, names what it cannot reach
//! or read and goes on, writes paths or a JSON array, removes entries
//! and runs the commands of `-exec` (MIKSPEC-0098.R33).

use crate::expression::{self, Entry, Expression, Type, World, child, last_component};
use crate::primaries::{self, COMMAND};
use mikros_cli::{FAILURE, INVALID, Paint, Parsed, Role, Run, SUCCESS, complain, time};
use std::fs::{self, Metadata};
use std::io::{self, BufWriter, IsTerminal, Stdout, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::process::{Command, ExitCode};

/// The bytes of paths after which a batch of `-exec ... {} +` runs.
const BATCH_BYTES: usize = 64 * 1024;

/// A batch of `-exec ... {} +`: its command and the paths so far.
struct Batch {
    command: Vec<String>,
    paths: Vec<String>,
    bytes: usize,
}

/// The walk: what it was asked, what it writes, and whether something
/// failed.
struct Finder<'a> {
    run: &'a Run,
    follow: bool,
    json: bool,
    paint: Paint,
    out: BufWriter<Stdout>,
    objects: Vec<String>,
    references: Vec<(i64, u32)>,
    batches: Vec<Batch>,
    now: i64,
    failed: bool,
}

pub fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if std::env::var("MSH_COMPLETE").is_ok_and(|v| v == "1") {
        let mut out = io::stdout().lock();
        for (value, what) in primaries::candidates(&arguments, &mikros_command::list) {
            let _ = match what.is_empty() {
                true => writeln!(out, "{value}"),
                false => writeln!(out, "{value}\t{what}"),
            };
        }
        return ExitCode::from(SUCCESS);
    }
    let (head, words) = primaries::split(&arguments);
    let run = match COMMAND.parse(head) {
        Parsed::Run(run) => run,
        Parsed::Help(_) => {
            mikros_cli::show_help(&primaries::help());
            return ExitCode::from(SUCCESS);
        }
        Parsed::Invalid(why) => {
            let terminal = io::stderr().is_terminal();
            let no_color = std::env::var("NO_COLOR").ok();
            let paint = Paint::new(None, terminal, no_color.as_deref());
            eprintln!("{}: {}", COMMAND.name, paint.paint(Role::Error, &why));
            return ExitCode::from(INVALID);
        }
    };
    let expression = match expression::parse(&words) {
        Ok(expression) => expression,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let json = run.has("--json");
    let mut finder = Finder {
        run: &run,
        follow: run.has("-L"),
        json,
        paint: match json {
            true => Paint::never(),
            false => Paint::stdout(&run),
        },
        out: BufWriter::new(io::stdout()),
        objects: Vec::new(),
        references: Vec::new(),
        batches: expression
            .batches
            .iter()
            .map(|command| Batch {
                command: command.clone(),
                paths: Vec::new(),
                bytes: 0,
            })
            .collect(),
        now: time::seconds(std::time::SystemTime::now()),
        failed: false,
    };
    for file in &expression.newer {
        match finder.stat(Path::new(file)) {
            Ok(m) => finder.references.push(modified(&m)),
            Err(e) => {
                complain(&COMMAND, &run, &format!("cannot access {file}: {e}"));
                return ExitCode::from(FAILURE);
            }
        }
    }
    let starts: Vec<String> = match run.operands.is_empty() {
        true => vec![".".into()],
        false => run.operands.clone(),
    };
    for start in &starts {
        match finder.stat(Path::new(start)) {
            Ok(m) => finder.visit(
                &expression,
                start.clone(),
                last_component(start),
                &m,
                0,
                &mut Vec::new(),
            ),
            Err(e) => finder.fail(&format!("cannot access {start}: {e}")),
        }
    }
    for index in 0..finder.batches.len() {
        finder.run_batch(index);
    }
    if json {
        let text = format!("[{}]\n", finder.objects.join(","));
        finder.failed |= finder.out.write_all(text.as_bytes()).is_err();
    }
    finder.failed |= finder.out.flush().is_err();
    match finder.failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

fn modified(metadata: &Metadata) -> (i64, u32) {
    let nanos = u32::try_from(metadata.mtime_nsec()).unwrap_or(0);
    (metadata.mtime(), nanos)
}

fn kind(metadata: &Metadata) -> Type {
    let t = metadata.file_type();
    if t.is_symlink() {
        Type::Link
    } else if t.is_dir() {
        Type::Folder
    } else if t.is_socket() {
        Type::Socket
    } else if t.is_file() {
        Type::File
    } else {
        Type::Other
    }
}

impl Finder<'_> {
    /// The metadata of `path`, of the link itself unless `-L` follows
    /// it; that of a link whose target is missing even with `-L`.
    fn stat(&self, path: &Path) -> io::Result<Metadata> {
        match self.follow {
            true => fs::metadata(path).or_else(|_| fs::symlink_metadata(path)),
            false => fs::symlink_metadata(path),
        }
    }

    /// Decides the expression for the entry at `path` and walks the
    /// folder it is, `depth` levels below a path given, inside the
    /// folders of `above`, by device and inode.
    fn visit(
        &mut self,
        expression: &Expression,
        path: String,
        name: String,
        metadata: &Metadata,
        depth: usize,
        above: &mut Vec<(u64, u64)>,
    ) {
        let entry = Entry {
            path,
            name,
            kind: kind(metadata),
            size: metadata.len(),
            modified: modified(metadata),
        };
        let tested = depth >= expression.mindepth;
        if tested && !expression.deletes {
            expression.root.decide(&entry, self);
        }
        let deeper = expression.maxdepth.is_none_or(|most| depth < most);
        if entry.kind == Type::Folder && deeper {
            self.walk(expression, &entry.path, metadata, depth, above);
        }
        if tested && expression.deletes {
            expression.root.decide(&entry, self);
        }
    }

    /// Walks the entries of the folder at `path` in the order of their
    /// names.
    fn walk(
        &mut self,
        expression: &Expression,
        path: &str,
        metadata: &Metadata,
        depth: usize,
        above: &mut Vec<(u64, u64)>,
    ) {
        let id = (metadata.dev(), metadata.ino());
        if self.follow && id.1 != 0 && above.contains(&id) {
            self.fail(&format!("{path} is inside itself through a link"));
            return;
        }
        let listing = match fs::read_dir(path) {
            Ok(listing) => listing,
            Err(e) => {
                self.fail(&format!("cannot read folder {path}: {e}"));
                return;
            }
        };
        let mut names = Vec::new();
        for item in listing {
            match item {
                Ok(item) => names.push(item.file_name().to_string_lossy().into_owned()),
                Err(e) => self.fail(&format!("reading {path}: {e}")),
            }
        }
        names.sort();
        above.push(id);
        for name in names {
            let at = child(path, &name);
            match self.stat(Path::new(&at)) {
                Ok(m) => self.visit(expression, at, name, &m, depth + 1, above),
                Err(e) => self.fail(&format!("cannot access {at}: {e}")),
            }
        }
        above.pop();
    }

    /// Runs the paths of the `index`th batch, if it has any.
    fn run_batch(&mut self, index: usize) {
        let paths = std::mem::take(&mut self.batches[index].paths);
        self.batches[index].bytes = 0;
        if paths.is_empty() {
            return;
        }
        let command = self.batches[index].command.clone();
        if self.command(&command, &paths) != Some(true) {
            self.failed = true;
        }
    }

    /// Runs `command` with `more` after it, once what was written is
    /// out; whether it exited with 0, or none when it did not start.
    fn command(&mut self, command: &[String], more: &[String]) -> Option<bool> {
        let _ = self.out.flush();
        let status = Command::new(&command[0])
            .args(&command[1..])
            .args(more)
            .status();
        match status {
            Ok(status) => Some(status.success()),
            Err(e) => {
                self.fail(&format!("cannot run {}: {e}", command[0]));
                None
            }
        }
    }

    fn fail(&mut self, message: &str) {
        let _ = self.out.flush();
        complain(&COMMAND, self.run, message);
        self.failed = true;
    }
}

impl World for Finder<'_> {
    fn empty_folder(&mut self, entry: &Entry) -> bool {
        fs::read_dir(&entry.path).is_ok_and(|mut listing| listing.next().is_none())
    }

    fn reference(&self, index: usize) -> (i64, u32) {
        self.references[index]
    }

    fn now(&self) -> i64 {
        self.now
    }

    fn print(&mut self, entry: &Entry, nul: bool) {
        if self.json {
            self.objects.push(entry.json());
            return;
        }
        let end = if nul { "\0" } else { "\n" };
        let shown = self.paint.paint(Role::Path, &entry.path);
        self.failed |= write!(self.out, "{shown}{end}").is_err();
    }

    fn delete(&mut self, entry: &Entry) -> bool {
        // The folder the walk is in stays, as with GNU find.
        if entry.path == "." {
            return true;
        }
        let removed = match entry.kind {
            Type::Folder => fs::remove_dir(&entry.path),
            _ => fs::remove_file(&entry.path),
        };
        match removed {
            Ok(()) => true,
            Err(e) => {
                self.fail(&format!("cannot delete {}: {e}", entry.path));
                false
            }
        }
    }

    fn exec(&mut self, command: &[String], entry: &Entry) -> bool {
        let command: Vec<String> = command
            .iter()
            .map(|word| word.replace("{}", &entry.path))
            .collect();
        self.command(&command, &[]) == Some(true)
    }

    fn batch(&mut self, index: usize, entry: &Entry) {
        let batch = &mut self.batches[index];
        batch.bytes += entry.path.len() + 1;
        batch.paths.push(entry.path.clone());
        if batch.bytes >= BATCH_BYTES {
            self.run_batch(index);
        }
    }
}
