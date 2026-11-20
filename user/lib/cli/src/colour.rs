//! Colours by role (MIKSPEC-0098.R70): whether a stream is coloured, from
//! `--color`, whether it is a terminal and `NO_COLOR`, and the terminal
//! palette, apart from msh's syntax colours, in the terminal's standard
//! colours so that they follow its theme. Names of file system entries
//! take the colour of the entry's type.

use crate::spec::Run;
use std::fs::{self, Metadata};
use std::io::IsTerminal;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// What a piece of output is, which gives its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The name of a file system entry, of the type given.
    Path(EntryType),
    /// Sizes, counts and IDs.
    Number,
    /// Free text, such as full names and messages.
    String,
    /// Text that a search matched.
    Match,
    /// Failures, denied and failed states, the severity `ERROR`.
    Error,
    /// The severity `WARNING`, and states such as restarting.
    Warning,
    /// Running and allowed states.
    Ok,
    /// Headers, `-`, times and debug records.
    Secondary,
}

impl Role {
    /// The SGR parameters of its colour, none for the terminal's default.
    fn sgr(self) -> Option<&'static str> {
        match self {
            Role::Path(EntryType::Folder) => Some("1;34"),
            Role::Path(EntryType::Executable) => Some("1;32"),
            Role::Path(EntryType::Link) => Some("36"),
            Role::Path(EntryType::Dangling) => Some("31"),
            Role::Path(EntryType::Other) | Role::Number | Role::String => None,
            Role::Match => Some("1;31"),
            Role::Error => Some("31"),
            Role::Warning => Some("33"),
            Role::Ok => Some("32"),
            Role::Secondary => Some("90"),
        }
    }
}

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

/// The type of a file system entry, which colours its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryType {
    Folder,
    /// A file whose stored mode has an execute bit.
    Executable,
    /// A symbolic link whose target exists.
    Link,
    /// A symbolic link whose target does not exist.
    Dangling,
    /// Any other entry, and a name of no entry.
    Other,
}

impl EntryType {
    /// The type of the entry at `path`, of a link itself.
    pub fn of(path: impl AsRef<Path>) -> EntryType {
        let path = path.as_ref();
        match fs::symlink_metadata(path) {
            Ok(metadata) => EntryType::from_metadata(path, &metadata),
            Err(_) => EntryType::Other,
        }
    }

    /// The type of the entry at `path` from its own `metadata`, not its
    /// target's, looking for the target only of a link.
    pub fn from_metadata(path: &Path, metadata: &Metadata) -> EntryType {
        match metadata.file_type().is_symlink() {
            true if fs::metadata(path).is_err() => EntryType::Dangling,
            _ => EntryType::from_mode(metadata.mode()),
        }
    }

    /// The type of an entry with `mode`, its type and permission bits, as
    /// archives store it: a link counts as one whose target exists.
    pub fn from_mode(mode: u32) -> EntryType {
        match mode & S_IFMT {
            S_IFDIR => EntryType::Folder,
            S_IFLNK => EntryType::Link,
            S_IFREG if mode & 0o111 != 0 => EntryType::Executable,
            _ => EntryType::Other,
        }
    }

    /// The type of the target of the link at `link` that reads `target`,
    /// which is relative to the link's folder unless it is absolute.
    pub fn of_target(link: impl AsRef<Path>, target: impl AsRef<Path>) -> EntryType {
        let folder = link.as_ref().parent().unwrap_or(Path::new(""));
        EntryType::of(folder.join(target))
    }
}

/// Whether one stream is coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paint(bool);

impl Paint {
    /// Colouring by `--color`'s `when`, for a stream that is a terminal
    /// when `terminal`, with `no_color` the value of `NO_COLOR`.
    pub fn new(when: Option<&str>, terminal: bool, no_color: Option<&str>) -> Paint {
        Paint(match when {
            Some("always") => true,
            Some("never") => false,
            _ => terminal && no_color.is_none_or(str::is_empty),
        })
    }

    /// Colouring of standard output for `run`.
    pub fn stdout(run: &Run) -> Paint {
        Paint::from_env(run.value("--color"), std::io::stdout().is_terminal())
    }

    /// Colouring of standard error for `run`.
    pub fn stderr(run: &Run) -> Paint {
        Paint::from_env(run.value("--color"), std::io::stderr().is_terminal())
    }

    fn from_env(when: Option<&str>, terminal: bool) -> Paint {
        let no_color = std::env::var("NO_COLOR").ok();
        Paint::new(when, terminal, no_color.as_deref())
    }

    /// No colour, for `--json` and for tests.
    pub const fn never() -> Paint {
        Paint(false)
    }

    pub fn on(self) -> bool {
        self.0
    }

    /// `text` in the colour of `role`, when the stream is coloured.
    pub fn paint(self, role: Role, text: &str) -> String {
        match role.sgr() {
            Some(sgr) if self.0 && !text.is_empty() => format!("\x1b[{sgr}m{text}\x1b[0m"),
            _ => text.to_string(),
        }
    }

    /// The type of the entry at `path` when the stream is coloured, and
    /// `Other`, without looking, when it is not.
    pub fn entry_type(self, path: impl AsRef<Path>) -> EntryType {
        match self.0 {
            true => EntryType::of(path),
            false => EntryType::Other,
        }
    }

    /// The type of the target of the link at `link` that reads `target`,
    /// as `entry_type`.
    pub fn target_type(self, link: impl AsRef<Path>, target: impl AsRef<Path>) -> EntryType {
        match self.0 {
            true => EntryType::of_target(link, target),
            false => EntryType::Other,
        }
    }

    /// `path` in the colour of the type of the entry it names.
    pub fn entry(self, path: impl AsRef<Path>) -> String {
        let path = path.as_ref();
        self.paint(Role::Path(self.entry_type(path)), &path.to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_by_option_terminal_and_no_color() {
        assert!(Paint::new(None, true, None).on());
        assert!(!Paint::new(None, false, None).on());
        assert!(!Paint::new(Some("auto"), true, Some("1")).on());
        assert!(Paint::new(Some("auto"), true, Some("")).on());
        assert!(Paint::new(Some("always"), false, Some("1")).on());
        assert!(!Paint::new(Some("never"), true, None).on());
    }

    #[test]
    fn paints_by_role_in_standard_colours() {
        let on = Paint::new(Some("always"), false, None);
        let folder = Role::Path(EntryType::Folder);
        assert_eq!(on.paint(folder, "a"), "\x1b[1;34ma\x1b[0m");
        let executable = Role::Path(EntryType::Executable);
        assert_eq!(on.paint(executable, "a"), "\x1b[1;32ma\x1b[0m");
        assert_eq!(
            on.paint(Role::Path(EntryType::Link), "a"),
            "\x1b[36ma\x1b[0m"
        );
        assert_eq!(
            on.paint(Role::Path(EntryType::Dangling), "a"),
            "\x1b[31ma\x1b[0m"
        );
        assert_eq!(on.paint(Role::Path(EntryType::Other), "a"), "a");
        assert_eq!(on.paint(Role::Number, "7"), "7");
        assert_eq!(on.paint(Role::String, "s"), "s");
        assert_eq!(on.paint(Role::Match, "m"), "\x1b[1;31mm\x1b[0m");
        assert_eq!(on.paint(Role::Error, "e"), "\x1b[31me\x1b[0m");
        assert_eq!(on.paint(Role::Warning, "w"), "\x1b[33mw\x1b[0m");
        assert_eq!(on.paint(Role::Ok, "o"), "\x1b[32mo\x1b[0m");
        assert_eq!(on.paint(Role::Secondary, "-"), "\x1b[90m-\x1b[0m");
        assert_eq!(on.paint(Role::Warning, ""), "");
        assert_eq!(Paint::never().paint(Role::Error, "x"), "x");
    }

    #[test]
    fn types_entries_by_mode() {
        assert_eq!(EntryType::from_mode(0o040755), EntryType::Folder);
        assert_eq!(EntryType::from_mode(0o100755), EntryType::Executable);
        assert_eq!(EntryType::from_mode(0o100010), EntryType::Executable);
        assert_eq!(EntryType::from_mode(0o100644), EntryType::Other);
        assert_eq!(EntryType::from_mode(0o120777), EntryType::Link);
        assert_eq!(EntryType::from_mode(0o020666), EntryType::Other);
    }

    #[test]
    fn types_entries_on_disk() {
        let root = std::env::temp_dir().join(format!("mikros-cli-colour-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let file = root.join("file");
        fs::write(&file, "").unwrap();
        let run = root.join("run");
        fs::write(&run, "").unwrap();
        fs::set_permissions(&run, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("file", root.join("link")).unwrap();
        std::os::unix::fs::symlink("gone", root.join("dangling")).unwrap();
        assert_eq!(EntryType::of(&root), EntryType::Folder);
        assert_eq!(EntryType::of(&file), EntryType::Other);
        assert_eq!(EntryType::of(&run), EntryType::Executable);
        assert_eq!(EntryType::of(root.join("link")), EntryType::Link);
        assert_eq!(EntryType::of(root.join("dangling")), EntryType::Dangling);
        assert_eq!(EntryType::of(root.join("none")), EntryType::Other);
        assert_eq!(
            EntryType::of_target(root.join("link"), "run"),
            EntryType::Executable
        );
        assert_eq!(EntryType::of_target(&file, &root), EntryType::Folder);
        let on = Paint::new(Some("always"), false, None);
        assert_eq!(
            on.entry(&root),
            format!("\x1b[1;34m{}\x1b[0m", root.display())
        );
        assert_eq!(Paint::never().entry_type(&root), EntryType::Other);
        fs::remove_dir_all(&root).unwrap();
    }
}
