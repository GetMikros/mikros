//! ln on Mikros: it creates links through the standard library
//! (MIKSPEC-0098.R33).

use crate::linking::{self, COMMAND, Link, Wrong};
use mikros_cli::{FAILURE, INVALID, Paint, Role, Run, SUCCESS, complain, start};
use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    if run.has("-r") && !run.has("-s") {
        complain(&COMMAND, &run, "-r needs -s");
        return ExitCode::from(INVALID);
    }
    let links = linking::plan(&run.operands, run.value("-t"), run.has("-T"), |name| {
        match run.has("-n") {
            true => fs::symlink_metadata(name),
            false => fs::metadata(name),
        }
        .is_ok_and(|m| m.is_dir())
    });
    let links = match links {
        Ok(links) => links,
        Err(Wrong::Usage(why)) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
        Err(Wrong::NotFolder(folder)) => {
            complain(&COMMAND, &run, &format!("{folder} is not a folder"));
            return ExitCode::from(FAILURE);
        }
    };
    let paint = Paint::stdout(&run);
    let mut failed = false;
    for link in &links {
        if let Err(why) = create(&run, link) {
            complain(&COMMAND, &run, &why);
            failed = true;
        } else if run.has("-v") {
            let shown = target(&run, link);
            let kind = match run.has("-s") {
                true => paint.target_type(&link.name, &shown),
                false => paint.entry_type(&link.target),
            };
            let _ = writeln!(
                std::io::stdout(),
                "{} -> {}",
                paint.entry(&link.name),
                paint.paint(Role::Path(kind), &shown)
            );
        }
    }
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

/// What a symbolic link holds: the target as given, or with `-r` made
/// relative to the link's folder.
fn target(run: &Run, link: &Link) -> String {
    if !run.has("-r") {
        return link.target.clone();
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let resolve = |path: &Path| {
        let path = linking::normalized(&cwd, path);
        fs::canonicalize(&path).unwrap_or(path)
    };
    let to = resolve(Path::new(&link.target));
    let name = linking::normalized(&cwd, Path::new(&link.name));
    let folder = resolve(name.parent().unwrap_or(Path::new("/")));
    linking::relative(&folder, &to).display().to_string()
}

fn create(run: &Run, link: &Link) -> Result<(), String> {
    let name = &link.name;
    if run.has("-f")
        && let Ok(existing) = fs::symlink_metadata(name)
    {
        if existing.is_dir() {
            return Err(format!("cannot overwrite folder {name}"));
        }
        let same = !run.has("-s")
            && fs::metadata(&link.target)
                .is_ok_and(|t| t.dev() == existing.dev() && t.ino() == existing.ino());
        if same {
            return Err(format!("{} and {name} are the same file", link.target));
        }
        fs::remove_file(name).map_err(|e| format!("cannot remove {name}: {e}"))?;
    }
    let made = match run.has("-s") {
        true => std::os::unix::fs::symlink(target(run, link), name),
        false => fs::hard_link(&link.target, name),
    };
    made.map_err(|e| format!("cannot create link {name} -> {}: {e}", link.target))
}
