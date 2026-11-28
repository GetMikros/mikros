//! The archives of prebuilt libraries (MIKSPEC-0062.R18): their entries are
//! checked, and the archive is unpacked and its files are checked against
//! the sysroot and the system, all before a file of it is used
//! (MIKSPEC-0062.R19, MIKSPEC-0062.R20).

use crate::elf;
use crate::ports::Port;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// The folders that an archive holds files in, laid out as `/system`.
const FOLDERS: [&str; 3] = ["include", "libraries", "share"];

/// Fails, naming `port`, unless every entry of `archive` is a folder or a
/// file below `include/`, `libraries/` or `share/`, with no name that
/// leaves them, and returns the names of the files.
pub fn entries(port: &Port, archive: &Path) -> Result<Vec<String>> {
    let output = Command::new("tar")
        .arg("-tvf")
        .arg(archive)
        .output()
        .context("running tar")?;
    if !output.status.success() {
        bail!(
            "{}: listing {} failed: {}",
            port.name,
            archive.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    check_entries(port, &String::from_utf8_lossy(&output.stdout))
}

/// The files of the verbose listing `listing` of an archive, checked as
/// `entries` says.
fn check_entries(port: &Port, listing: &str) -> Result<Vec<String>> {
    let mut files = Vec::new();
    for line in listing.lines() {
        // `-rw-r--r-- 0/0 1234 1970-01-01 00:00 libraries/libx.dlib`
        let kind = line.chars().next().unwrap_or(' ');
        let mut rest = line;
        for _ in 0..5 {
            rest = rest
                .trim_start()
                .split_once(char::is_whitespace)
                .map(|(_, rest)| rest)
                .with_context(|| {
                    format!("{}: cannot read the archive entry {line:?}", port.name)
                })?;
        }
        let name = rest.trim().to_owned();
        // The root folder of an archive made from inside its tree.
        if kind == 'd' && name.trim_end_matches('/') == "." {
            continue;
        }
        let path = Path::new(&name);
        let plain = path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        let first = path
            .components()
            .find_map(|c| match c {
                Component::Normal(n) => n.to_str(),
                _ => None,
            })
            .unwrap_or("");
        if !plain || !FOLDERS.contains(&first) {
            bail!(
                "{}: the entry {name} is outside include/, libraries/ and share/",
                port.name
            );
        }
        match kind {
            'd' => {}
            '-' => {
                let file = name.trim_start_matches("./").to_owned();
                if files.contains(&file) {
                    bail!("{}: the entry {file} is in the archive twice", port.name);
                }
                files.push(file);
            }
            _ => bail!(
                "{}: the entry {name} is neither a file nor a folder",
                port.name
            ),
        }
    }
    files.sort();
    Ok(files)
}

/// Unpacks `archive`, whose entries `entries` accepted, into the new folder
/// `dest`, which is not a part of the sysroot or of an image.
pub fn unpack(port: &Port, archive: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        fs::remove_dir_all(dest).with_context(|| format!("removing {}", dest.display()))?;
    }
    fs::create_dir_all(dest)?;
    let status = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .context("running tar")?;
    if !status.success() {
        bail!("{}: unpacking {} failed", port.name, archive.display());
    }
    Ok(())
}

/// The shared libraries of the folder `libraries` of `root`, by `DT_SONAME`,
/// with the path of each.
fn libraries_in(root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut found = BTreeMap::new();
    let dir = root.join("libraries");
    let Ok(read) = fs::read_dir(&dir) else {
        return Ok(found);
    };
    for entry in read {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("dlib") || !path.is_file() {
            continue;
        }
        if let Some(name) = elf::soname(&path)? {
            found.insert(name, path);
        }
    }
    Ok(found)
}

/// The ABI level that the C library `libc.dlib` of the sysroot gives, which
/// is the target level of its ABI note.
fn sysroot_level(sysroot: &Path) -> Result<u32> {
    let path = sysroot.join("libraries/libc.dlib");
    let data = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(elf::abi_levels(&data)
        .with_context(|| path.display().to_string())?
        .1)
}

/// Fails, naming `port`, when the unpacked archive `tree` of a prebuilt
/// library does not fit the sysroot `sysroot` and the files `taken` of the
/// system and of other ports, given as paths in `/system`: a file that would
/// replace one of them, a shared library whose soname is not its file name,
/// one without an ABI note or with a minimum level above that of the C
/// library of the sysroot, a needed library that is neither in the sysroot
/// nor in the archive, and a symbol that is undefined and not weak and that
/// none of the libraries that it needs defines (MIKSPEC-0062.R19).
pub fn check_tree(
    port: &Port,
    files: &[String],
    tree: &Path,
    sysroot: &Path,
    taken: &BTreeSet<String>,
) -> Result<()> {
    let name = &port.name;
    for file in files {
        if taken.contains(file) {
            bail!("{name}: the file {file} would replace a file of the system or of another port");
        }
    }
    let level = sysroot_level(sysroot).with_context(|| name.clone())?;
    let own = libraries_in(tree)?;
    for file in files.iter().filter(|f| f.starts_with("libraries/")) {
        let Some(base) = file.strip_prefix("libraries/") else {
            continue;
        };
        if !base.ends_with(".dlib") {
            continue;
        }
        let found = elf::soname(&tree.join(file)).with_context(|| format!("{name}: {file}"))?;
        if found.as_deref() != Some(base) {
            bail!("{name}: the soname of {file} is {found:?}, not {base}");
        }
        let data = fs::read(tree.join(file))?;
        let (minimum, _) = elf::abi_levels(&data).with_context(|| format!("{name}: {file}"))?;
        if minimum > level {
            bail!(
                "{name}: {file} needs the ABI level {minimum}, and the C library of the \
                 sysroot gives {level}"
            );
        }
    }
    let system = libraries_in(sysroot)?;
    let library = |soname: &str| own.get(soname).or_else(|| system.get(soname));
    for (soname, path) in &own {
        // The libraries that this one needs, directly and through the
        // libraries that those need.
        let mut closure: BTreeMap<String, &Path> = BTreeMap::new();
        let mut queue = VecDeque::from([(soname.clone(), path.as_path())]);
        while let Some((current, at)) = queue.pop_front() {
            if closure.insert(current.clone(), at).is_some() {
                continue;
            }
            let data = fs::read(at)?;
            for needed in elf::dynamic(&data)
                .with_context(|| format!("{name}: {current}"))?
                .needed
            {
                let Some(next) = library(&needed) else {
                    bail!(
                        "{name}: {current} needs {needed}, which is in neither the sysroot nor the archive"
                    );
                };
                queue.push_back((needed, next.as_path()));
            }
        }
        let mut defined = BTreeSet::new();
        for at in closure.values() {
            let data = fs::read(at)?;
            defined.extend(elf::dynamic_symbols(&data)?.defined);
        }
        let data = fs::read(path)?;
        for symbol in elf::dynamic_symbols(&data)?.undefined {
            if !defined.contains(&symbol) {
                bail!(
                    "{name}: {soname} needs the symbol {symbol}, which none of its libraries defines"
                );
            }
        }
    }
    Ok(())
}

/// A prebuilt library whose archive was fetched, listed and unpacked.
pub struct Prepared {
    pub port: Port,
    /// The folder that the archive is unpacked into.
    pub tree: PathBuf,
    /// The files of the archive, by path in `/system`.
    pub files: Vec<String>,
}

/// Fetches, lists and unpacks the archive of `port`, unless it is
/// unpacked already, below `target/prebuilt/<name>/tree`.
pub fn prepare(port: Port) -> Result<Prepared> {
    let archive = crate::ports::fetch_prebuilt(&port)?;
    let files = entries(&port, &archive)?;
    let base = crate::workspace_root()
        .join("target/prebuilt")
        .join(&port.name);
    let tree = base.join("tree");
    let marker = base.join("unpacked");
    if fs::read_to_string(&marker).ok().as_deref() != Some(port.sha256.as_str()) {
        unpack(&port, &archive, &tree)?;
        fs::write(&marker, &port.sha256)?;
    }
    Ok(Prepared { port, tree, files })
}

/// Every prebuilt library that `user/ports` pins, prepared.
pub fn prepare_all() -> Result<Vec<Prepared>> {
    crate::ports::prebuilt_ports()?
        .into_iter()
        .map(prepare)
        .collect()
}

/// Whether a file of an archive, by its path in `/system`, goes into the
/// sysroot: `include/`, `libraries/` and `share/cmake/`
/// (MIKSPEC-0062.R19).
fn for_sysroot(file: &str) -> bool {
    file.starts_with("include/")
        || file.starts_with("libraries/")
        || file.starts_with("share/cmake/")
}

/// Whether a file of an archive, by its path in `/system`, goes into the
/// system version of an image: `libraries/` and `share/`.
fn for_images(file: &str) -> bool {
    file.starts_with("libraries/") || file.starts_with("share/")
}

/// The files that `record` says an earlier run installed for the port
/// `name`, one path to a line.
fn recorded(record: &Path, name: &str) -> BTreeSet<String> {
    fs::read_to_string(record.join(format!("{name}.files")))
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Whether the files `a` and `b` hold the same bytes.
fn same_bytes(a: &Path, b: &Path) -> bool {
    matches!((fs::read(a), fs::read(b)), (Ok(x), Ok(y)) if x == y)
}

/// Installs the files of the prebuilt libraries `prepared` in `sysroot`,
/// after every check of MIKSPEC-0062.R19 passed for all of them: nothing
/// is written before that, so that a library that fails leaves the sysroot
/// as it was (MIKSPEC-0062.R20). What an earlier run installed for the same
/// port is not a replacement; `record` keeps the names of those files.
pub fn install_in(sysroot: &Path, record: &Path, prepared: &[Prepared]) -> Result<()> {
    let mut owner: BTreeMap<&str, &str> = BTreeMap::new();
    for item in prepared {
        for file in &item.files {
            if let Some(other) = owner.insert(file, &item.port.name) {
                bail!(
                    "{}: the file {file} would replace a file of the port {other}",
                    item.port.name
                );
            }
        }
    }
    for item in prepared {
        let own = recorded(record, &item.port.name);
        let mut taken = BTreeSet::new();
        for file in item.files.iter().filter(|f| for_sysroot(f)) {
            let at = sysroot.join(file);
            // A file with the archive's own bytes is no replacement, so a
            // sysroot restored from a cache that does not hold `record`
            // installs again.
            if at.exists() && !own.contains(file) && !same_bytes(&at, &item.tree.join(file)) {
                taken.insert(file.clone());
            }
        }
        check_tree(&item.port, &item.files, &item.tree, sysroot, &taken)?;
    }
    fs::create_dir_all(record)?;
    for item in prepared {
        let mut installed = String::new();
        for file in item.files.iter().filter(|f| for_sysroot(f)) {
            let data = fs::read(item.tree.join(file))
                .with_context(|| format!("{}: reading {file}", item.port.name))?;
            crate::system_libraries::write_if_changed(&sysroot.join(file), &data)?;
            installed.push_str(file);
            installed.push('\n');
        }
        fs::write(record.join(format!("{}.files", item.port.name)), installed)?;
    }
    Ok(())
}

/// Installs the prebuilt libraries that `user/ports` pins in the sysroot.
pub fn install_all() -> Result<()> {
    let prepared = prepare_all()?;
    if prepared.is_empty() {
        return Ok(());
    }
    // The record is kept in the sysroot, so that a cache that restores the
    // sysroot restores it as well.
    let sysroot = crate::sysroot::dir();
    install_in(&sysroot, &sysroot.join(".prebuilt"), &prepared)
}

/// The folders with the shared libraries of the prebuilt libraries that
/// `user/ports` pin, for the linker of the programs of the user workspace:
/// a program links such a library by its file name, and the sysroot holds
/// the library only after `cargo xtask sysroot`.
pub fn library_dirs() -> Result<Vec<PathBuf>> {
    Ok(prepare_all()?
        .into_iter()
        .filter(|item| item.files.iter().any(|f| f.starts_with("libraries/")))
        .map(|item| item.tree.join("libraries"))
        .collect())
}

/// The members that install the prebuilt libraries in the system version
/// of an image of the edition `edition`, each by its name in the image:
/// the files of `libraries/` and `share/` that the `[editions]` of the port
/// give to that edition. Fails, naming the port, for an archive that does
/// not fit the sysroot as `check_tree` says, for a table that names a file
/// that the archive does not hold and for a member of `existing` that a file
/// would replace.
pub fn image_members(
    edition: crate::build::Edition,
    existing: &[(String, PathBuf)],
) -> Result<Vec<(String, PathBuf)>> {
    image_members_in(&prepare_all()?, edition, existing, &crate::sysroot::dir())
}

/// `image_members` for the libraries `prepared` and the sysroot `sysroot`.
fn image_members_in(
    prepared: &[Prepared],
    edition: crate::build::Edition,
    existing: &[(String, PathBuf)],
    sysroot: &Path,
) -> Result<Vec<(String, PathBuf)>> {
    let mut members: Vec<(String, PathBuf)> = Vec::new();
    for item in prepared {
        // The checks of the sysroot install apply to what an image holds
        // as well (MIKSPEC-0062.R19, MIKSPEC-0062.R20).
        check_tree(
            &item.port,
            &item.files,
            &item.tree,
            sysroot,
            &BTreeSet::new(),
        )?;
        let files: Vec<&str> = item
            .files
            .iter()
            .map(String::as_str)
            .filter(|f| for_images(f))
            .collect();
        item.port.check_editions(&files)?;
        for file in files {
            if !item.port.installed_in(file, edition) {
                continue;
            }
            let at = format!("system/{file}");
            if existing.iter().chain(&members).any(|(have, _)| *have == at) {
                bail!(
                    "{}: the file {file} would replace a file of the system or of another port",
                    item.port.name
                );
            }
            members.push((at, item.tree.join(file)));
        }
    }
    Ok(members)
}

/// The name and the `commit` of each prebuilt library that an image of the
/// edition `edition` holds, for `cargo xtask image` to print
/// (MIKSPEC-0062.R19).
pub fn held(edition: crate::build::Edition) -> Result<Vec<(String, String)>> {
    let mut held = Vec::new();
    for item in prepare_all()? {
        let holds = item
            .files
            .iter()
            .filter(|f| for_images(f))
            .any(|f| item.port.installed_in(f, edition));
        if let (true, Some(prebuilt)) = (holds, &item.port.prebuilt) {
            held.push((item.port.name.clone(), prebuilt.commit.clone()));
        }
    }
    Ok(held)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::fixtures::{Symbol, dynamic_elf};

    fn port() -> Port {
        let text = "[prebuilt]\nurl = \"u\"\nsha256 = \"s\"\nrepository = \"r\"\n\
                    commit = \"0123456789abcdef0123456789abcdef01234567\"\n\
                    workflow = \"w\"\nsysroot = \"s\"\ntoolchain = \"t\"\n";
        crate::ports::parse_for_tests("elegance", text)
    }

    /// A temporary folder with the files `(path, content)`.
    fn make(files: &[(&str, Vec<u8>)]) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "xtask-prebuilt-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (path, data) in files {
            let at = dir.join(path);
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(at, data).unwrap();
        }
        dir
    }

    /// The sysroot of the tests: a C library with the ABI level 2 and one
    /// symbol, `puts`.
    fn sysroot() -> PathBuf {
        make(&[(
            "libraries/libc.dlib",
            dynamic_elf(
                Some("libc.dlib"),
                &[],
                &[Symbol("puts", true, false)],
                Some((1, 2)),
            ),
        )])
    }

    fn reason(
        files: &[(&str, Vec<u8>)],
        sysroot: &Path,
        taken: &[&str],
    ) -> std::result::Result<(), String> {
        let tree = make(files);
        let names: Vec<String> = files.iter().map(|(p, _)| (*p).to_owned()).collect();
        let taken = taken.iter().map(|s| (*s).to_owned()).collect();
        check_tree(&port(), &names, &tree, sysroot, &taken).map_err(|e| format!("{e:#}"))
    }

    #[test]
    fn accepts_a_library_that_fits() {
        let lib = dynamic_elf(
            Some("libx.dlib"),
            &["libc.dlib"],
            &[
                Symbol("x", true, false),
                Symbol("puts", false, false),
                Symbol("maybe", false, true),
            ],
            Some((1, 2)),
        );
        let files = [
            ("libraries/libx.dlib", lib),
            ("include/x.h", b"#define X".to_vec()),
        ];
        assert_eq!(reason(&files, &sysroot(), &[]), Ok(()));
    }

    #[test]
    fn refuses_a_file_that_would_replace_another() {
        let files = [("share/x/data", b"1".to_vec())];
        let error = reason(&files, &sysroot(), &["share/x/data"]).unwrap_err();
        assert!(
            error.contains("elegance") && error.contains("would replace"),
            "{error}"
        );
    }

    #[test]
    fn refuses_a_soname_that_is_not_the_file_name() {
        let lib = dynamic_elf(Some("liby.dlib"), &[], &[], Some((1, 1)));
        let error = reason(&[("libraries/libx.dlib", lib)], &sysroot(), &[]).unwrap_err();
        assert!(error.contains("soname"), "{error}");
        let none = dynamic_elf(None, &[], &[], Some((1, 1)));
        assert!(reason(&[("libraries/libx.dlib", none)], &sysroot(), &[]).is_err());
    }

    #[test]
    fn refuses_a_library_without_an_abi_note_or_above_the_level_of_the_c_library() {
        let plain = dynamic_elf(Some("libx.dlib"), &[], &[], None);
        let error = reason(&[("libraries/libx.dlib", plain)], &sysroot(), &[]).unwrap_err();
        assert!(error.contains("no ABI note"), "{error}");
        let high = dynamic_elf(Some("libx.dlib"), &[], &[], Some((3, 3)));
        let error = reason(&[("libraries/libx.dlib", high)], &sysroot(), &[]).unwrap_err();
        assert!(
            error.contains("ABI level 3") && error.contains("gives 2"),
            "{error}"
        );
    }

    #[test]
    fn refuses_a_needed_library_that_is_nowhere() {
        let lib = dynamic_elf(Some("libx.dlib"), &["libgone.dlib"], &[], Some((1, 1)));
        let error = reason(&[("libraries/libx.dlib", lib)], &sysroot(), &[]).unwrap_err();
        assert!(error.contains("libgone.dlib"), "{error}");
        // One that the archive holds, and one that the other needs in turn.
        let x = dynamic_elf(Some("libx.dlib"), &["liby.dlib"], &[], Some((1, 1)));
        let y = dynamic_elf(Some("liby.dlib"), &["libc.dlib"], &[], Some((1, 1)));
        let both = [("libraries/libx.dlib", x), ("libraries/liby.dlib", y)];
        assert_eq!(reason(&both, &sysroot(), &[]), Ok(()));
        let z = dynamic_elf(Some("liby.dlib"), &["libmissing.dlib"], &[], Some((1, 1)));
        let x = dynamic_elf(Some("libx.dlib"), &["liby.dlib"], &[], Some((1, 1)));
        let error = reason(
            &[("libraries/libx.dlib", x), ("libraries/liby.dlib", z)],
            &sysroot(),
            &[],
        )
        .unwrap_err();
        assert!(error.contains("libmissing.dlib"), "{error}");
    }

    #[test]
    fn refuses_an_undefined_symbol_that_no_needed_library_defines() {
        let lib = dynamic_elf(
            Some("libx.dlib"),
            &["libc.dlib"],
            &[Symbol("nowhere", false, false)],
            Some((1, 1)),
        );
        let error = reason(&[("libraries/libx.dlib", lib)], &sysroot(), &[]).unwrap_err();
        assert!(error.contains("nowhere"), "{error}");
        // A library that it does not need does not count.
        let other = dynamic_elf(
            Some("liby.dlib"),
            &[],
            &[Symbol("def", true, false)],
            Some((1, 1)),
        );
        let x = dynamic_elf(
            Some("libx.dlib"),
            &["libc.dlib"],
            &[Symbol("def", false, false)],
            Some((1, 1)),
        );
        let error = reason(
            &[("libraries/libx.dlib", x), ("libraries/liby.dlib", other)],
            &sysroot(),
            &[],
        )
        .unwrap_err();
        assert!(error.contains("def"), "{error}");
    }

    #[test]
    fn reads_and_unpacks_an_archive() {
        let tree = make(&[
            ("libraries/libx.dlib", b"x".to_vec()),
            ("include/x.h", b"h".to_vec()),
        ]);
        let archive = tree.with_extension("tar.gz");
        let made = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&tree)
            .args(["include", "libraries"])
            .status()
            .unwrap();
        assert!(made.success());
        let files = entries(&port(), &archive).unwrap();
        assert_eq!(files, ["include/x.h", "libraries/libx.dlib"]);
        let dest = tree.with_extension("out");
        unpack(&port(), &archive, &dest).unwrap();
        assert_eq!(fs::read(dest.join("include/x.h")).unwrap(), b"h");
        // An archive with a loose file is refused.
        let loose = tree.with_extension("loose.tar.gz");
        fs::write(tree.join("loose"), b"1").unwrap();
        let made = Command::new("tar")
            .arg("-czf")
            .arg(&loose)
            .arg("-C")
            .arg(&tree)
            .args(["include", "loose"])
            .status()
            .unwrap();
        assert!(made.success());
        assert!(entries(&port(), &loose).is_err());
        for path in [&tree, &dest] {
            let _ = fs::remove_dir_all(path);
        }
        let _ = fs::remove_file(&archive);
        let _ = fs::remove_file(&loose);
    }

    #[test]
    fn accepts_the_entries_of_the_three_folders_only() {
        let listing = "drwxr-xr-x 0/0 0 1970-01-01 00:00 libraries/\n\
                       -rwxr-xr-x 0/0 5 1970-01-01 00:00 libraries/libx.dlib\n\
                       -rw-r--r-- 0/0 5 1970-01-01 00:00 ./include/x.h\n";
        let files = check_entries(&port(), listing).unwrap();
        assert_eq!(files, ["include/x.h", "libraries/libx.dlib"]);
        let root = format!("drwxr-xr-x 0/0 0 1970-01-01 00:00 ./\n{listing}");
        assert_eq!(check_entries(&port(), &root).unwrap(), files);
        for bad in [
            "-rw-r--r-- 0/0 5 1970-01-01 00:00 bin/x\n",
            "-rw-r--r-- 0/0 5 1970-01-01 00:00 /libraries/x\n",
            "-rw-r--r-- 0/0 5 1970-01-01 00:00 libraries/../etc/x\n",
            "lrwxrwxrwx 0/0 0 1970-01-01 00:00 libraries/x -> /etc/passwd\n",
            "hrwxr-xr-x 0/0 0 1970-01-01 00:00 libraries/x link to libraries/y\n",
            "-rw-r--r-- 0/0 5 1970-01-01 00:00 include/x.h\n\
             -rw-r--r-- 0/0 5 1970-01-01 00:00 ./include/x.h\n",
        ] {
            assert!(check_entries(&port(), bad).is_err(), "{bad}");
        }
    }

    /// A prepared prebuilt library of the tree `files`.
    fn prepared(name: &str, extra: &str, files: &[(&str, Vec<u8>)]) -> Prepared {
        let text = format!(
            "[prebuilt]\nurl = \"u\"\nsha256 = \"s\"\nrepository = \"r\"\n\
             commit = \"0123456789abcdef0123456789abcdef01234567\"\n\
             workflow = \"w\"\nsysroot = \"s\"\ntoolchain = \"t\"\n{extra}"
        );
        let tree = make(files);
        Prepared {
            port: crate::ports::parse_for_tests(name, &text),
            tree,
            files: files.iter().map(|(p, _)| (*p).to_owned()).collect(),
        }
    }

    fn lib() -> Vec<u8> {
        dynamic_elf(
            Some("libx.dlib"),
            &["libc.dlib"],
            &[Symbol("puts", false, false)],
            Some((1, 1)),
        )
    }

    #[test]
    fn installs_include_libraries_and_the_cmake_files_in_the_sysroot_again_and_again() {
        let sysroot = sysroot();
        let record = make(&[]);
        let one = prepared(
            "elegance",
            "",
            &[
                ("libraries/libx.dlib", lib()),
                ("include/x.h", b"h".to_vec()),
                ("share/cmake/X/XConfig.cmake", b"c".to_vec()),
                ("share/x/data", b"d".to_vec()),
            ],
        );
        install_in(&sysroot, &record, std::slice::from_ref(&one)).unwrap();
        assert!(sysroot.join("libraries/libx.dlib").is_file());
        assert_eq!(fs::read(sysroot.join("include/x.h")).unwrap(), b"h");
        assert!(sysroot.join("share/cmake/X/XConfig.cmake").is_file());
        // The rest of share/ goes into images only.
        assert!(!sysroot.join("share/x/data").exists());
        // A second run finds the port's own files and replaces nothing.
        install_in(&sysroot, &record, std::slice::from_ref(&one)).unwrap();
    }

    #[test]
    fn a_sysroot_without_the_record_takes_the_same_files_but_not_changed_ones() {
        let sysroot = sysroot();
        let record = make(&[]);
        let one = prepared(
            "elegance",
            "",
            &[
                ("libraries/libx.dlib", lib()),
                ("include/x.h", b"h".to_vec()),
            ],
        );
        install_in(&sysroot, &record, std::slice::from_ref(&one)).unwrap();
        // The cache holds the sysroot, not the record.
        fs::remove_dir_all(&record).unwrap();
        install_in(&sysroot, &record, std::slice::from_ref(&one)).unwrap();
        assert!(record.join("elegance.files").is_file());
        fs::remove_dir_all(&record).unwrap();
        fs::write(sysroot.join("include/x.h"), b"changed").unwrap();
        let error = install_in(&sysroot, &record, std::slice::from_ref(&one)).unwrap_err();
        assert!(format!("{error:#}").contains("would replace"), "{error:#}");
        assert_eq!(fs::read(sysroot.join("include/x.h")).unwrap(), b"changed");
    }

    #[test]
    fn a_library_that_fails_leaves_the_sysroot_as_it_was() {
        let sysroot = sysroot();
        let record = make(&[]);
        let good = prepared("good", "", &[("include/g.h", b"g".to_vec())]);
        let bad = prepared(
            "bad",
            "",
            &[(
                "libraries/libx.dlib",
                dynamic_elf(Some("liby.dlib"), &[], &[], Some((1, 1))),
            )],
        );
        let error = install_in(&sysroot, &record, &[good, bad]).unwrap_err();
        assert!(format!("{error:#}").contains("bad"), "{error:#}");
        assert!(!sysroot.join("include/g.h").exists());
    }

    #[test]
    fn a_file_of_the_sysroot_or_of_another_port_is_not_replaced() {
        let sysroot = make(&[("include/own.h", b"1".to_vec())]);
        let record = make(&[]);
        let theirs = prepared("theirs", "", &[("include/own.h", b"2".to_vec())]);
        // The sysroot has libc.dlib too; the tree is checked against it.
        fs::create_dir_all(sysroot.join("libraries")).unwrap();
        fs::write(
            sysroot.join("libraries/libc.dlib"),
            dynamic_elf(Some("libc.dlib"), &[], &[], Some((1, 2))),
        )
        .unwrap();
        let error = install_in(&sysroot, &record, &[theirs]).unwrap_err();
        assert!(format!("{error:#}").contains("would replace"), "{error:#}");
        let a = prepared("a", "", &[("include/same.h", b"1".to_vec())]);
        let b = prepared("b", "", &[("include/same.h", b"2".to_vec())]);
        let error = install_in(&sysroot, &record, &[a, b]).unwrap_err();
        assert!(format!("{error:#}").contains("port a"), "{error:#}");
    }

    #[test]
    fn images_get_the_files_of_the_editions_that_hold_them() {
        use crate::build::Edition;
        let editions = "[editions]\n\"libraries/libx.dlib\" = [\"desktop\"]\n";
        let item = prepared(
            "elegance",
            editions,
            &[
                ("libraries/libx.dlib", lib()),
                ("include/x.h", b"h".to_vec()),
                ("share/x/data", b"d".to_vec()),
            ],
        );
        let sysroot = sysroot();
        let names = |edition| -> Vec<String> {
            image_members_in(std::slice::from_ref(&item), edition, &[], &sysroot)
                .unwrap()
                .into_iter()
                .map(|(at, _)| at)
                .collect()
        };
        assert_eq!(
            names(Edition::Desktop),
            ["system/libraries/libx.dlib", "system/share/x/data"]
        );
        assert_eq!(names(Edition::Server), ["system/share/x/data"]);
        // A member that is there already is not replaced.
        let have = vec![("system/share/x/data".to_owned(), PathBuf::from("x"))];
        let error = image_members_in(
            std::slice::from_ref(&item),
            Edition::Server,
            &have,
            &sysroot,
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("elegance")
                && format!("{error:#}").contains("would replace"),
            "{error:#}"
        );
        // A library that does not fit the sysroot is refused here too.
        let bad = prepared(
            "bad",
            "",
            &[(
                "libraries/libx.dlib",
                dynamic_elf(Some("liby.dlib"), &[], &[], Some((1, 1))),
            )],
        );
        let error = image_members_in(&[bad], Edition::Desktop, &[], &sysroot).unwrap_err();
        assert!(format!("{error:#}").contains("bad"), "{error:#}");
        // A table that names a file that the archive lacks fails.
        let wrong = prepared(
            "wrong",
            "[editions]\n\"libraries/gone.dlib\" = [\"desktop\"]\n",
            &[("libraries/libx.dlib", lib())],
        );
        assert!(wrong.port.check_editions(&["libraries/libx.dlib"]).is_err());
    }
}
