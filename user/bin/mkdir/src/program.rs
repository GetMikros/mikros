//! mkdir on Mikros: it creates folders through the standard library
//! (MIKSPEC-0098.R33), and sets the mode of `-m` exactly, whatever the
//! file creation mask.

use crate::making::{COMMAND, Mode, prefixes};
use mikros_cli::{FAILURE, INVALID, Paint, Role, Run, SUCCESS, complain, start};
use std::fs::{self, DirBuilder, Permissions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::process::ExitCode;

unsafe extern "C" {
    fn umask(mask: u32) -> u32;
}

struct Maker<'a> {
    run: &'a Run,
    paint: Paint,
    /// The mode of `-m`, as it is to be set.
    mode: Option<u32>,
    failed: bool,
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let mode = match run.value("-m").map(|m| (m, Mode::parse(m))) {
        None => None,
        Some((_, Some(mode))) => {
            // SAFETY: setting the mask and setting it back has no other
            // effect.
            let mask = unsafe { umask(0) };
            // SAFETY: as above.
            unsafe { umask(mask) };
            Some(mode.apply(0o777, true, mask))
        }
        Some((text, None)) => {
            complain(&COMMAND, &run, &format!("invalid mode {text}"));
            return ExitCode::from(INVALID);
        }
    };
    let mut maker = Maker {
        run: &run,
        paint: Paint::stdout(&run),
        mode,
        failed: false,
    };
    for operand in &run.operands {
        let path = Path::new(operand);
        match run.has("-p") {
            true => maker.with_parents(path),
            false => {
                maker.create(path, true);
            }
        }
    }
    match maker.failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

impl Maker<'_> {
    /// Creates `path` and the folders above it that are missing.
    fn with_parents(&mut self, path: &Path) {
        let all = prefixes(path);
        for (i, folder) in all.iter().enumerate() {
            if fs::metadata(folder).is_ok_and(|m| m.is_dir()) {
                continue;
            }
            if !self.create(folder, i + 1 == all.len()) {
                return;
            }
        }
    }

    /// Creates the folder `path`, with the mode of `-m` when `last`, and
    /// whether it did, or found it made meanwhile with `-p`.
    fn create(&mut self, path: &Path, last: bool) -> bool {
        let shown = path.display();
        let mode = self.mode.filter(|_| last);
        let made = DirBuilder::new()
            .mode(mode.unwrap_or(0o777) & 0o777)
            .create(path);
        match made {
            Ok(()) => {}
            Err(e)
                if e.kind() == ErrorKind::AlreadyExists && self.run.has("-p") && path.is_dir() =>
            {
                return true;
            }
            Err(e) => {
                self.fail(&format!("cannot create folder {shown}: {e}"));
                return false;
            }
        }
        if self.run.has("-v") {
            let path = self.paint.paint(Role::Path, &shown.to_string());
            let _ = writeln!(std::io::stdout(), "created folder {path}");
        }
        if let Some(mode) = mode
            && let Err(e) = fs::set_permissions(path, Permissions::from_mode(mode))
        {
            self.fail(&format!("cannot set the mode of {shown}: {e}"));
        }
        true
    }

    fn fail(&mut self, message: &str) {
        complain(&COMMAND, self.run, message);
        self.failed = true;
    }
}
