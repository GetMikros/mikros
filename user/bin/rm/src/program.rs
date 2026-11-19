//! rm on Mikros: it removes entries through the standard library
//! (MIKSPEC-0098.R33), after refusing the root and the system version.

use crate::removal::{self, Asking, COMMAND, Refusal, Settings};
use mikros_cli::{EntryType, FAILURE, INVALID, Paint, Role, Run, SUCCESS, complain, start};
use std::fs;
use std::io::{BufRead, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

struct Remover<'a> {
    run: &'a Run,
    settings: Settings,
    paint: Paint,
    failed: bool,
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let settings = Settings::of(&run);
    if run.operands.is_empty() {
        if settings.force {
            return ExitCode::from(SUCCESS);
        }
        complain(&COMMAND, &run, "missing operand <path>...");
        return ExitCode::from(INVALID);
    }
    let mut remover = Remover {
        run: &run,
        settings,
        paint: Paint::stdout(&run),
        failed: false,
    };
    if let Some(question) = settings.once(run.operands.len())
        && !remover.ask(&question)
    {
        return ExitCode::from(SUCCESS);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    for operand in &run.operands {
        remover.operand(&cwd, operand);
    }
    match remover.failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

impl Remover<'_> {
    fn operand(&mut self, cwd: &Path, operand: &str) {
        let path = Path::new(operand);
        let refusal = match removal::dots(operand) {
            true => Some(Refusal::Dots),
            false => removal::refused(&removal::resolved(cwd, path, |p| fs::canonicalize(p).ok())),
        };
        if let Some(refusal) = refusal {
            return self.fail(&format!("refusing to remove {operand}: {}", refusal.why()));
        }
        match fs::symlink_metadata(path) {
            Err(e) if e.kind() == ErrorKind::NotFound && self.settings.force => {}
            Err(e) => self.fail(&format!("cannot remove {operand}: {e}")),
            Ok(m) if m.is_dir() => self.folder(path, true),
            Ok(m) => {
                let what = match m.file_type().is_symlink() {
                    true => "link",
                    false => "file",
                };
                self.file(path, what);
            }
        }
    }

    /// Removes the folder `path`: what it holds too with `-r`, and only
    /// when it is empty with `-d`. `top` when an operand names it.
    fn folder(&mut self, path: &Path, top: bool) {
        let shown = path.display();
        if !self.settings.recursive {
            if !self.settings.dir {
                return self.fail(&format!("cannot remove {shown}: is a folder"));
            }
            return self.remove_folder(path);
        }
        if self.settings.asking == Asking::Each
            && !self.ask(&format!("descend into folder {shown}"))
        {
            return;
        }
        match fs::read_dir(path) {
            Err(e) => {
                let verb = if top { "remove" } else { "read" };
                return self.fail(&format!("cannot {verb} {shown}: {e}"));
            }
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Err(e) => self.fail(&format!("cannot read {shown}: {e}")),
                        Ok(entry) => self.entry(&entry.path()),
                    }
                }
            }
        }
        self.remove_folder(path);
    }

    /// Removes an entry found in a folder, without following links.
    fn entry(&mut self, path: &Path) {
        match fs::symlink_metadata(path) {
            Err(e) => self.fail(&format!("cannot remove {}: {e}", path.display())),
            Ok(m) if m.is_dir() => self.folder(path, false),
            Ok(m) if m.file_type().is_symlink() => self.file(path, "link"),
            Ok(_) => self.file(path, "file"),
        }
    }

    fn remove_folder(&mut self, path: &Path) {
        let shown = path.display();
        if self.settings.asking == Asking::Each && !self.ask(&format!("remove folder {shown}")) {
            return;
        }
        match fs::remove_dir(path) {
            Ok(()) => self.removed("removed folder", path, EntryType::Folder),
            Err(e) => self.fail(&format!("cannot remove {shown}: {e}")),
        }
    }

    /// Removes the file or link `path`, `what` it is.
    fn file(&mut self, path: &Path, what: &str) {
        let shown = path.display();
        if self.settings.asking == Asking::Each && !self.ask(&format!("remove {what} {shown}")) {
            return;
        }
        let kind = self.paint.entry_type(path);
        match fs::remove_file(path) {
            Ok(()) => self.removed("removed", path, kind),
            Err(e) => self.fail(&format!("cannot remove {shown}: {e}")),
        }
    }

    fn removed(&self, what: &str, path: &Path, kind: EntryType) {
        if self.settings.verbose {
            let path = self
                .paint
                .paint(Role::Path(kind), &path.display().to_string());
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{what} {path}");
        }
    }

    /// Asks `question` on standard error, and whether standard input
    /// answered yes.
    fn ask(&self, question: &str) -> bool {
        eprint!("{}: {question}? ", COMMAND.name);
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        match std::io::stdin().lock().read_line(&mut answer) {
            Ok(_) => removal::yes(&answer),
            Err(_) => false,
        }
    }

    fn fail(&mut self, message: &str) {
        complain(&COMMAND, self.run, message);
        self.failed = true;
    }
}
