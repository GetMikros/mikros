//! The files of a demos directory, which `cargo xtask image --demos` installs
//! in the system of the image, so that programs and libraries that are built
//! outside this repository can be run on the desktop edition.
//!
//! The directory is laid out as `/system`: its `bin/` holds programs, which
//! go to `/system/bin`, its `libraries/` holds shared libraries, each named
//! after its soname with the suffix `.dlib`, which go to
//! `/system/libraries`, and its `share/` holds files that go to
//! `/system/share`. Nothing else is accepted, so that a demo cannot change
//! what the system does, and no file of the demos replaces one of the
//! system's: the root partition's tree refuses that when it installs them.
//! Each program and library records its ABI levels (MIKSPEC-0068.R22).

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The top-level folders of a demos directory.
const FOLDERS: [&str; 3] = ["bin", "libraries", "share"];

/// A file of the demos, and where it goes.
#[derive(Debug, PartialEq, Eq)]
pub struct Demo {
    /// Its path in the system version, such as `bin/demo`.
    pub at: String,
    /// The file on the host.
    pub source: PathBuf,
}

/// The files of the demos directory `dir`, in the order of their paths, or
/// the first reason why it cannot be installed.
pub fn collect(dir: &Path) -> Result<Vec<Demo>> {
    let meta = fs::metadata(dir).with_context(|| format!("reading {}", dir.display()))?;
    if !meta.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    let mut files = BTreeMap::new();
    for entry in sorted(dir)? {
        let name = file_name(&entry)?;
        if !FOLDERS.contains(&name.as_str()) {
            bail!(
                "{} is not one of {}, the folders of the demos",
                entry.display(),
                FOLDERS.join(", ")
            );
        }
        let flat = name != "share";
        collect_folder(&entry, &name, flat, &mut files)?;
    }
    // The system's paths are not case folded here, so a clash of two demos
    // that differ in case is only a trap for whoever copies the image to
    // another file system.
    let mut seen = BTreeMap::new();
    for at in files.keys() {
        if let Some(other) = seen.insert(at.to_ascii_lowercase(), at) {
            bail!("{other} and {at} differ only in case");
        }
    }
    Ok(files
        .into_iter()
        .map(|(at, source)| Demo { at, source })
        .collect())
}

/// Adds the files of `folder`, which is the top-level folder `top` of the
/// demos, to `files`: only the folder's own files when it is `flat`.
fn collect_folder(
    folder: &Path,
    at: &str,
    flat: bool,
    files: &mut BTreeMap<String, PathBuf>,
) -> Result<()> {
    if !fs::metadata(folder)?.is_dir() {
        bail!("{} is not a folder", folder.display());
    }
    for entry in sorted(folder)? {
        let name = file_name(&entry)?;
        let path = format!("{at}/{name}");
        // A link to a file is followed: its content is what gets installed.
        // A link to a folder is not, which also rules out loops.
        let kind = fs::symlink_metadata(&entry)?.file_type();
        let meta = fs::metadata(&entry)
            .with_context(|| format!("{} is a link to nothing", entry.display()))?;
        if kind.is_symlink() && meta.is_dir() {
            bail!("{} is a link to a folder", entry.display());
        }
        if meta.is_dir() {
            if flat {
                bail!("{} is a folder, and {at} holds only files", entry.display());
            }
            collect_folder(&entry, &path, flat, files)?;
        } else if meta.is_file() {
            if at.starts_with("libraries") {
                check_library(&entry, &name)?;
            }
            if at.starts_with("libraries") || at.starts_with("bin") {
                check_abi_note(&entry)?;
            }
            files.insert(path, entry);
        } else {
            bail!("{} is neither a file nor a folder", entry.display());
        }
    }
    Ok(())
}

/// The paths of the entries of `dir`, in order.
fn sorted(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        entries.push(entry?.path());
    }
    entries.sort();
    Ok(entries)
}

/// The name of the last component of `path`, if it is one that a file of the
/// system can have: letters, digits and `._+-`, and not starting with a dot.
fn file_name(path: &Path) -> Result<String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("{} has a name that is not UTF-8", path.display()))?;
    let allowed = |c: char| c.is_ascii_alphanumeric() || "._+-".contains(c);
    if name.is_empty() || name.starts_with('.') || !name.chars().all(allowed) {
        bail!(
            "{} has a name of other characters than letters, digits and ._+-, or starts with a dot",
            path.display()
        );
    }
    Ok(name.to_owned())
}

/// Fails unless `path`, a file of `libraries/` named `name`, is a library of
/// the system: a name with the suffix `.dlib`, and the soname of that name.
fn check_library(path: &Path, name: &str) -> Result<()> {
    if !name.ends_with(".dlib") {
        bail!(
            "{} is in libraries/, whose files end in .dlib",
            path.display()
        );
    }
    let found = crate::build_systems::soname(path)?;
    check_soname(name, found.as_deref())
        .with_context(|| format!("{}: link it with -Wl,-soname,{name}", path.display()))
}

/// Fails unless the ELF file `path` records its ABI levels, in a note of the
/// name `Mikros` and the type 1, whose target level is not below its minimum
/// (MIKSPEC-0068.R22).
fn check_abi_note(path: &Path) -> Result<()> {
    let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    crate::elf::abi_levels(&data).with_context(|| path.display().to_string())?;
    Ok(())
}

/// Fails unless the soname `found` of a library is its file name `name`, which
/// is what the loader looks for.
fn check_soname(name: &str, found: Option<&str>) -> Result<()> {
    if found != Some(name) {
        bail!("its soname is {found:?}, not {name}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::elf::fixtures::note_elf as elf;

    /// A demos directory of the given files, each with its content: the
    /// programs are ELF files with an ABI note.
    fn make(files: &[&str]) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "xtask-demos-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        for file in files {
            let path = dir.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            if file.starts_with("bin/") {
                fs::write(&path, elf(Some((1, 1)))).unwrap();
            } else {
                fs::write(&path, b"content").unwrap();
            }
        }
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn reason(dir: &Path) -> String {
        format!("{:#}", collect(dir).unwrap_err())
    }

    #[test]
    fn collects_the_files_in_the_order_of_their_paths() {
        let dir = make(&["share/fonts/a.ttf", "bin/zeta", "bin/alpha", "share/b.txt"]);
        let ats: Vec<String> = collect(&dir).unwrap().into_iter().map(|d| d.at).collect();
        assert_eq!(
            ats,
            ["bin/alpha", "bin/zeta", "share/b.txt", "share/fonts/a.ttf"]
        );
        let first = &collect(&dir).unwrap()[0];
        assert_eq!(first.source, dir.join("bin/alpha"));
    }

    #[test]
    fn refuses_what_is_not_one_of_the_folders() {
        assert!(reason(&make(&["config/session.yaml"])).contains("config"));
        assert!(reason(&make(&["programs/x.yaml"])).contains("programs"));
        assert!(reason(&make(&["loose"])).contains("loose"));
    }

    #[test]
    fn keeps_bin_and_libraries_flat() {
        assert!(reason(&make(&["bin/tests/demo"])).contains("holds only files"));
        assert!(reason(&make(&["libraries/sub/libx.dlib"])).contains("holds only files"));
        assert!(collect(&make(&["share/a/b/c.txt"])).is_ok());
    }

    #[test]
    fn refuses_names_that_could_escape_or_clash() {
        for bad in ["bin/.hidden", "bin/a b", "bin/a:b", "bin/é"] {
            assert!(collect(&make(&[bad])).is_err(), "{bad}");
        }
        assert!(collect(&make(&["bin/libc++-test_1.0"])).is_ok());
    }

    #[test]
    fn refuses_names_that_differ_only_in_case() {
        assert!(reason(&make(&["bin/Demo", "bin/demo"])).contains("only in case"));
    }

    #[test]
    fn refuses_a_program_that_has_no_abi_note() {
        let dir = make(&["bin/good"]);
        assert!(collect(&dir).is_ok());
        fs::write(dir.join("bin/bad"), elf(None)).unwrap();
        let reason = reason(&dir);
        assert!(reason.contains("bin/bad") && reason.contains("no ABI note"));
    }

    #[test]
    fn refuses_a_library_without_the_suffix() {
        assert!(reason(&make(&["libraries/libx.so"])).contains(".dlib"));
    }

    #[test]
    fn a_library_has_its_file_name_as_its_soname() {
        assert!(check_soname("libx.1.dlib", Some("libx.1.dlib")).is_ok());
        assert!(
            check_soname("libx.1.dlib", Some("libx.2.dlib"))
                .unwrap_err()
                .to_string()
                .contains("libx.2.dlib")
        );
        assert!(check_soname("libx.1.dlib", None).is_err());
    }

    #[test]
    fn follows_a_link_to_a_file_and_refuses_a_link_to_a_folder() {
        let dir = make(&["bin/real", "share/f/x"]);
        std::os::unix::fs::symlink(dir.join("bin/real"), dir.join("bin/alias")).unwrap();
        let ats: Vec<String> = collect(&dir).unwrap().into_iter().map(|d| d.at).collect();
        assert!(ats.contains(&"bin/alias".to_owned()));
        std::os::unix::fs::symlink(dir.join("share/f"), dir.join("share/g")).unwrap();
        assert!(reason(&dir).contains("link to a folder"));
    }

    #[test]
    fn refuses_a_directory_that_is_not_one() {
        let dir = make(&["bin/x"]);
        assert!(collect(&dir.join("bin/x")).is_err());
        assert!(collect(&dir.join("missing")).is_err());
    }
}
