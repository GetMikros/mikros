//! mktemp on Mikros: it creates the file or folder through the standard
//! library, trying names until one is not taken, and writes its path.

use crate::template::{self, COMMAND};
use mikros_cli::{FAILURE, INVALID, Paint, SUCCESS, complain, start};
use std::collections::hash_map::RandomState;
use std::fs::{DirBuilder, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

/// How many names it tries before it gives up.
const TRIES: usize = 1000;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let tmpdir = std::env::var("TMPDIR").ok();
    let plan = match template::plan(&run, tmpdir.as_deref()) {
        Ok(plan) => plan,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let state = RandomState::new();
    let mut count = 0u64;
    let mut random = || {
        count += 1;
        let mut hasher = state.build_hasher();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        hasher.write_u128(now.as_nanos());
        hasher.write_u32(std::process::id());
        hasher.write_u64(count);
        hasher.finish()
    };
    let mut last = None;
    for _ in 0..TRIES {
        let path = plan.path(Some(&mut random));
        match make(&path, run.has("-d"), run.has("-u")) {
            Ok(()) => {
                let shown = Paint::stdout(&run).entry(&path);
                return match writeln!(std::io::stdout(), "{shown}") {
                    Ok(()) => ExitCode::from(SUCCESS),
                    Err(_) => ExitCode::from(FAILURE),
                };
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => {
                last = Some(e);
                break;
            }
        }
    }
    if !run.has("-q") {
        let what = if run.has("-d") { "folder" } else { "file" };
        let why = last.map_or("every name is taken".into(), |e| e.to_string());
        let template = plan.path(None);
        let message = format!("cannot make a {what} from {}: {why}", template.display());
        complain(&COMMAND, &run, &message);
    }
    ExitCode::from(FAILURE)
}

/// Makes a folder, with `folder`, or a file at `path`, only for the
/// caller, or with `dry` only checks that nothing is there.
fn make(path: &Path, folder: bool, dry: bool) -> std::io::Result<()> {
    if dry {
        return match std::fs::symlink_metadata(path) {
            Ok(_) => Err(ErrorKind::AlreadyExists.into()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
    }
    match folder {
        true => DirBuilder::new().mode(0o700).create(path),
        false => OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map(drop),
    }
}
