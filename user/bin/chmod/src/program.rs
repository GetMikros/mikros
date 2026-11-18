//! chmod on Mikros: it reads each file's stored mode through the
//! standard library and sets the whole new mode with `set_permissions`,
//! which the C library sends as `SetAttributes` (MIKSPEC-0049.R23).

use crate::mode::{self, COMMAND, Mode, NOTE, Source};
use mikros_cli::{FAILURE, INVALID, Paint, Role, Run, SUCCESS, complain, start};
use std::fs::{self, Permissions};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::ExitCode;

unsafe extern "C" {
    fn umask(mask: u32) -> u32;
}

/// What the run asks and what it saw.
struct Changer<'a> {
    run: &'a Run,
    mode: Mode,
    umask: u32,
    failed: bool,
    /// Whether a change left every execute bit as it was.
    kept_execute: bool,
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let (source, paths) = match mode::request(&run) {
        Ok(request) => request,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let mode = match source {
        Source::Mode(mode) => mode,
        Source::Reference(file) => match fs::metadata(&file) {
            Ok(m) => Mode::Octal(m.mode() & 0o7777),
            Err(e) => {
                complain(&COMMAND, &run, &format!("cannot access {file}: {e}"));
                return ExitCode::from(FAILURE);
            }
        },
    };
    let mut changer = Changer {
        run: &run,
        mode,
        umask: creation_mask(),
        failed: false,
        kept_execute: false,
    };
    for path in &paths {
        changer.change(Path::new(path), true);
    }
    if changer.kept_execute {
        let paint = Paint::stderr(&run);
        eprintln!("{}: {}", COMMAND.name, paint.paint(Role::Warning, NOTE));
    }
    match changer.failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

/// The process's file creation mask, which `umask` gives only by setting
/// another, so it is set back at once.
fn creation_mask() -> u32 {
    // SAFETY: the calls take no pointers.
    unsafe {
        let mask = umask(0);
        umask(mask);
        mask
    }
}

impl Changer<'_> {
    /// Changes `path`, an operand when `operand`, which a link is
    /// followed for, and with `-R` the entries of a folder, leaving
    /// links inside alone.
    fn change(&mut self, path: &Path, operand: bool) {
        let metadata = match operand {
            true => fs::metadata(path),
            false => fs::symlink_metadata(path),
        };
        let metadata = match metadata {
            Ok(m) => m,
            Err(e) => return self.fail(&format!("cannot access {}: {e}", path.display())),
        };
        if metadata.file_type().is_symlink() {
            return;
        }
        let folder = metadata.is_dir();
        // The entries are read before the folder's mode changes, which
        // could stop them being read.
        let entries = match folder && self.run.has("-R") {
            true => self.entries(path),
            false => Vec::new(),
        };
        let old = metadata.mode() & 0o7777;
        let new = self.mode.apply(old, folder, self.umask);
        match fs::set_permissions(path, Permissions::from_mode(new)) {
            Ok(()) => {
                self.kept_execute |= mode::keeps_execute(old, new);
                if self.run.has("-v") {
                    print!("{}", mode::verbose(&path.display().to_string(), old, new));
                }
            }
            Err(e) => self.fail(&format!("changing the mode of {}: {e}", path.display())),
        }
        for entry in entries {
            self.change(&path.join(entry), false);
        }
    }

    /// The names in `folder`, in order.
    fn entries(&mut self, folder: &Path) -> Vec<String> {
        let listing = match fs::read_dir(folder) {
            Ok(listing) => listing,
            Err(e) => {
                self.fail(&format!("cannot open folder {}: {e}", folder.display()));
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

    fn fail(&mut self, message: &str) {
        complain(&COMMAND, self.run, message);
        self.failed = true;
    }
}
