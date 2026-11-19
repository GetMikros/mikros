//! df on Mikros: it asks `QueryFilesystem` (MIKSPEC-0049.R32) of the
//! root of each entry of the process's namespace (MIKSPEC-0060.R13), or
//! of each path named, whose volume's path is the highest folder above
//! it on the same entry and file system.

use crate::table::{self, COMMAND, Volume};
use mikros_cli::{FAILURE, Paint, SUCCESS, complain, start};
use mikros_command::node;
use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let json = run.has("--json");
    let mut failed = false;
    let mut volumes: Vec<Volume> = Vec::new();
    if run.operands.is_empty() {
        let entries = match node::entries() {
            Ok(entries) => entries,
            Err(e) => {
                complain(&COMMAND, &run, &format!("cannot list the namespace: {e}"));
                failed = true;
                Vec::new()
            }
        };
        for (root, connection) in entries {
            // What does not answer, or holds no space, such as `/dev`, is
            // no volume.
            let Ok(volume) = describe(&root, &connection) else {
                continue;
            };
            if volume.size > 0 && !volumes.iter().any(|v| v.same(&volume)) {
                volumes.push(volume);
            }
        }
    }
    for path in &run.operands {
        match holder(Path::new(path)) {
            Ok(volume) => volumes.push(volume),
            Err(why) => {
                complain(&COMMAND, &run, &format!("{path}: {why}"));
                failed = true;
            }
        }
    }
    let text = match json {
        true => table::json(&volumes),
        false => table::text(&volumes, run.has("-h"), Paint::stdout(&run)),
    };
    let written = std::io::stdout().write_all(text.as_bytes());
    match failed || written.is_err() {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

/// The volume at `path`, as its file system describes it.
fn query(path: &Path) -> Result<Volume, String> {
    let node = node::open(path, true).map_err(|e| e.to_string())?;
    describe(path, &node)
}

/// The volume at `path`, whose connection is `node`.
fn describe(path: &Path, node: &node::NodeClient) -> Result<Volume, String> {
    let info = node::filesystem(node).map_err(|e| table::words(&format!("{e:?}")))?;
    Ok(Volume {
        path: path.display().to_string(),
        kind: info.r#type,
        label: info.label,
        id: info.id,
        size: info.total_bytes,
        free: info.free_bytes,
        read_only: info.read_only,
    })
}

/// The volume that holds `path`, with the highest folder above it that
/// lies on the same entry and the same file system as its path.
fn holder(path: &Path) -> Result<Volume, String> {
    let absolute = lexical(&std::path::absolute(path).map_err(|e| e.to_string())?);
    let device = fs::metadata(&absolute).map_err(|e| e.to_string())?.dev();
    let volume = query(&absolute)?;
    let mut root = absolute.clone();
    while let Some(parent) = root.parent() {
        let same_entry = fs::metadata(parent).is_ok_and(|m| m.dev() == device);
        let same = same_entry && query(parent).is_ok_and(|v| holds_same(&v, &volume));
        if !same {
            break;
        }
        root = parent.to_path_buf();
    }
    Ok(Volume {
        path: root.display().to_string(),
        ..volume
    })
}

/// Whether `a` and `b` are the same file system, whose free space may
/// change between the questions.
fn holds_same(a: &Volume, b: &Volume) -> bool {
    a.kind == b.kind
        && a.id == b.id
        && a.label == b.label
        && a.size == b.size
        && a.read_only == b.read_only
}

/// `path` without `.` components and with `..` removing the component
/// before it, as the C library resolves paths.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => out.push(name),
            _ => {}
        }
    }
    out
}
