//! stat on Mikros: it reads each path's metadata through the standard
//! library, names owners and users through the C library, which asks the
//! accounts service, and asks the path's node connection what the caller
//! may do and for its access control lists (MIKSPEC-0098.R33).

use crate::facts::{self, AclLine, COMMAND, Facts, Form, Tag, Type};
use mikros_cli::{FAILURE, Paint, Run, SUCCESS, complain, start};
use mikros_command::node;
use std::collections::BTreeMap;
use std::ffi::{CStr, c_char};
use std::fs::{self, Metadata};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::process::ExitCode;

/// The name shown for user 0, as other Mikros tools show it.
const SYSTEM: &str = "system";
/// The raw values of the tags of `mikros.fs.AclTag`.
const OWNER: u32 = 1;
const USER: u32 = 2;
const GROUP: u32 = 3;
const APP: u32 = 4;
const MASK: u32 = 5;
const OTHER: u32 = 6;

/// musl's `struct passwd`.
#[repr(C)]
struct Passwd {
    name: *const c_char,
    password: *const c_char,
    uid: u32,
    gid: u32,
    gecos: *const c_char,
    home: *const c_char,
    shell: *const c_char,
}

unsafe extern "C" {
    fn getpwuid(uid: u32) -> *const Passwd;
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let form = Form::of(&run);
    let paint = match form {
        Form::Text => Paint::stdout(&run),
        _ => Paint::never(),
    };
    let mut names = Names::default();
    let mut failed = false;
    let mut out = String::new();
    let mut all = Vec::new();
    for path in &run.operands {
        let Some(facts) = gather(&run, path, &mut names) else {
            failed = true;
            continue;
        };
        match &form {
            Form::Text => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&facts::text(&facts, paint));
            }
            Form::Format(format) => out.push_str(&facts::format(format, &facts)),
            Form::Json => all.push(facts),
        }
    }
    if form == Form::Json {
        out = facts::json(&all);
    }
    let written = std::io::stdout().write_all(out.as_bytes());
    match failed || written.is_err() {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

/// The facts of `path`, following a link with `-L`, or `None` after
/// saying why there are none.
fn gather(run: &Run, path: &str, names: &mut Names) -> Option<Facts> {
    let follow = run.has("-L");
    let metadata = match follow {
        true => fs::metadata(path),
        false => fs::symlink_metadata(path),
    };
    let metadata = match metadata {
        Ok(m) => m,
        Err(e) => {
            complain(&COMMAND, run, &format!("cannot stat {path}: {e}"));
            return None;
        }
    };
    let kind = kind(&metadata);
    let target = match kind {
        Type::Link => fs::read_link(path).ok().map(|t| t.display().to_string()),
        _ => None,
    };
    let (access, acl, default_acl) = lists(Path::new(path), kind, names);
    Some(Facts {
        name: path.into(),
        target,
        kind,
        size: metadata.size(),
        space: metadata.blocks().saturating_mul(512),
        inode: metadata.ino(),
        links: metadata.nlink(),
        access,
        owner: names.user(metadata.uid()),
        mode: metadata.mode() & 0o7777,
        // The C library's `stat` carries no creation time; std says so.
        created: metadata.created().ok().map(mikros_cli::time::seconds),
        modified: metadata.mtime(),
        accessed: metadata.atime(),
        changed: metadata.ctime(),
        acl,
        default_acl,
    })
}

fn kind(metadata: &Metadata) -> Type {
    let t = metadata.file_type();
    if t.is_symlink() {
        Type::Link
    } else if t.is_dir() {
        Type::Folder
    } else if t.is_socket() {
        Type::Socket
    } else {
        Type::File
    }
}

type Lists = (Option<String>, Option<Vec<AclLine>>, Option<Vec<AclLine>>);

/// What the caller may do with the node at `path`, the link itself for a
/// link, its access list and, for a folder, its default list; each
/// unknown where the node does not open or answer.
fn lists(path: &Path, kind: Type, names: &mut Names) -> Lists {
    let Ok(node) = node::open(path, kind != Type::Link) else {
        return (None, None, None);
    };
    let access = node::access(&node).ok().map(node::rwx);
    let mut list = |default: bool| -> Option<Vec<AclLine>> {
        let entries = node::acl(&node, default).ok()?;
        let lines = entries
            .into_iter()
            .map(|e| AclLine {
                tag: names.tag(e.tag.into_raw(), e.id),
                rights: node::rwx(e.rights),
            })
            .collect();
        Some(lines)
    };
    let acl = list(false);
    let default_acl = match kind {
        Type::Folder => list(true),
        _ => None,
    };
    (access, acl, default_acl)
}

/// The names of users and the app IDs of app numbers, as they are
/// looked up once each.
#[derive(Default)]
struct Names {
    users: BTreeMap<u32, String>,
    apps: BTreeMap<u32, String>,
    /// The service directory, once an app is looked up.
    directory: Option<Option<mikros_command::Directory>>,
}

impl Names {
    /// The name of user `user`: `system` for 0, or its number when the
    /// accounts service does not know it.
    fn user(&mut self, user: u32) -> String {
        if user == 0 {
            return SYSTEM.into();
        }
        self.users
            .entry(user)
            .or_insert_with(|| lookup(user))
            .clone()
    }

    /// The app ID of app number `app` from the broker, or the number
    /// when it does not know it.
    fn app(&mut self, app: u32) -> String {
        if let Some(id) = self.apps.get(&app) {
            return id.clone();
        }
        let directory = self.directory.get_or_insert_with(|| {
            mikros_command::Directory::new(std::os::mikros::directory().as_raw_handle())
        });
        let id = directory
            .as_ref()
            .and_then(|d| d.app_id(app))
            .unwrap_or_else(|| app.to_string());
        self.apps.insert(app, id.clone());
        id
    }

    /// The tag of an entry, raw as the protocol gives it, for `id`.
    fn tag(&mut self, raw: u32, id: u32) -> Tag {
        match raw {
            OWNER => Tag::Owner,
            USER => Tag::User(self.user(id)),
            GROUP => Tag::Group,
            APP => Tag::App(self.app(id)),
            MASK => Tag::Mask,
            OTHER => Tag::Other,
            other => Tag::Unknown(other),
        }
    }
}

/// The name of user `user` from the C library, or its number.
fn lookup(user: u32) -> String {
    // SAFETY: the call takes no pointers.
    let entry = unsafe { getpwuid(user) };
    // SAFETY: a non-null entry and its name stay valid until the next
    // call of getpwuid.
    match unsafe { entry.as_ref() } {
        Some(entry) if !entry.name.is_null() => unsafe { CStr::from_ptr(entry.name) }
            .to_string_lossy()
            .into_owned(),
        _ => user.to_string(),
    }
}
