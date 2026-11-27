//! The third-party projects of user space, each pinned in
//! `user/ports/<name>/port.toml` with its Mikros changes beside it:
//!
//! - `[source]`: the `url` of the archive of its source, the archive's
//!   `sha256`, and the directory `dir` that holds the source once it is
//!   unpacked;
//! - `[build]`: the `options` that a Meson project is configured with;
//! - `[tests]`: `data`, the directories of its source that its tests read;
//! - `[editions]`: for a library or file of the port that only some editions
//!   of the system install, its path in `/system` and the names of those
//!   editions (MIKSPEC-0023.R53). What is not named is in every edition.
//!
//! A port may instead be a prebuilt library, which `[prebuilt]` pins as an
//! archive that was built elsewhere, with its `url` and `sha256` and the
//! record of where it came from, and no `[source]`, `[build]` or `[tests]`
//! (MIKSPEC-0062.R18).
//!
//! Its changes are patch files, in `patches/`. The
//! Meson projects are built here, the others by modules of their own.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::PathBuf;

/// The ports whose `[editions]` tables the build reads, because it takes
/// files from them for the system image.
pub const EDITION_PORTS: [&str; 4] = ["wayland", "vulkan-loader", "mesa", "llvm"];

/// The pin of a prebuilt library: the archive and the record of where it
/// came from, which xtask does not check beyond its form
/// (MIKSPEC-0062.R18).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prebuilt {
    pub url: String,
    pub sha256: String,
    pub repository: String,
    /// The commit that the library was built from, 40 lowercase hexadecimal
    /// digits.
    pub commit: String,
    pub workflow: String,
    pub sysroot: String,
    pub toolchain: String,
}

/// A ported project, or a prebuilt library.
#[derive(Debug)]
pub struct Port {
    /// Its name, which names its directories.
    pub name: String,
    /// Its directory in `user/ports`.
    pub dir: PathBuf,
    pub url: String,
    /// SHA-256 of the archive at `url`.
    pub sha256: String,
    /// The directory that holds the source once it is unpacked; empty for a
    /// prebuilt library.
    pub source: String,
    /// The pin of a prebuilt library, which has no source.
    pub prebuilt: Option<Prebuilt>,
    /// The options it is configured with.
    pub options: Vec<String>,
    /// The directories of its source, relative to it, that its tests
    /// read.
    pub test_data: Vec<String>,
    /// The editions that install a path of `/system`, for the paths that
    /// only some editions install.
    pub editions: std::collections::BTreeMap<String, Vec<String>>,
}

impl Port {
    /// Fails if the `[editions]` table names a path that is not one of
    /// `files`, the files that the build takes from the port, given as paths
    /// in `/system`.
    pub fn check_editions(&self, files: &[&str]) -> Result<()> {
        match self.editions.keys().find(|k| !files.contains(&k.as_str())) {
            Some(path) => bail!(
                "the [editions] table of {} names {path}, which is not a file of the port",
                self.name
            ),
            None => Ok(()),
        }
    }

    /// Whether the edition `edition` installs the file `path` of the
    /// port, given as a path in `/system`, such as `libraries/libx.dlib`.
    pub fn installed_in(&self, path: &str, edition: crate::build::Edition) -> bool {
        self.editions
            .get(path)
            .is_none_or(|names| names.iter().any(|n| n == edition.name()))
    }
}

/// The port `name`, from its `port.toml`.
pub fn load(name: &str) -> Result<Port> {
    let dir = crate::workspace_root().join("user/ports").join(name);
    let path = dir.join("port.toml");
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    parse(name, dir, &text).with_context(|| format!("in {}", path.display()))
}

/// Whether `text` is 40 lowercase hexadecimal digits.
fn is_commit(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The table `[prebuilt]`: every key, none empty, and a commit of 40
/// lowercase hexadecimal digits.
fn parse_prebuilt(table: &toml::Table) -> Result<Prebuilt> {
    let keys = [
        "url",
        "sha256",
        "repository",
        "commit",
        "workflow",
        "sysroot",
        "toolchain",
    ];
    only(table, "[prebuilt]", &keys)?;
    let mut values = Vec::new();
    for key in keys {
        let value = string(table, key).with_context(|| format!("[prebuilt] has no `{key}`"))?;
        if value.is_empty() {
            bail!("[prebuilt] has an empty `{key}`");
        }
        values.push(value);
    }
    let [
        url,
        sha256,
        repository,
        commit,
        workflow,
        sysroot,
        toolchain,
    ]: [String; 7] = values.try_into().expect("seven keys");
    if !is_commit(&commit) {
        bail!("the `commit` of [prebuilt] is not 40 lowercase hexadecimal digits");
    }
    Ok(Prebuilt {
        url,
        sha256,
        repository,
        commit,
        workflow,
        sysroot,
        toolchain,
    })
}

fn parse(name: &str, dir: PathBuf, text: &str) -> Result<Port> {
    let table: toml::Table = text.parse()?;
    only(
        &table,
        "the file",
        &["source", "build", "tests", "editions", "prebuilt"],
    )?;
    let editions = parse_editions(&table)?;
    if let Some(prebuilt) = table.get("prebuilt") {
        // A prebuilt library has neither a source nor a build nor tests.
        for other in ["source", "build", "tests"] {
            if table.contains_key(other) {
                bail!("a prebuilt library has no [{other}] table");
            }
        }
        let prebuilt = parse_prebuilt(prebuilt.as_table().context("[prebuilt] is not a table")?)?;
        return Ok(Port {
            name: name.to_owned(),
            dir,
            url: prebuilt.url.clone(),
            sha256: prebuilt.sha256.clone(),
            source: String::new(),
            prebuilt: Some(prebuilt),
            options: Vec::new(),
            test_data: Vec::new(),
            editions,
        });
    }
    let source = table
        .get("source")
        .and_then(|s| s.as_table())
        .context("no [source] table")?;
    only(source, "[source]", &["url", "sha256", "dir"])?;
    let section = |key: &str, keys: &[&str]| -> Result<Vec<String>> {
        match table.get(key) {
            None => Ok(Vec::new()),
            Some(t) => {
                let t = t
                    .as_table()
                    .with_context(|| format!("[{key}] is not a table"))?;
                only(t, &format!("[{key}]"), keys)?;
                strings(t, keys[0])
            }
        }
    };
    Ok(Port {
        name: name.to_owned(),
        dir,
        url: string(source, "url")?,
        sha256: string(source, "sha256")?,
        source: string(source, "dir")?,
        prebuilt: None,
        options: section("build", &["options"])?,
        test_data: section("tests", &["data"])?,
        editions,
    })
}

/// The table `[editions]`: for each path, the editions that install it.
fn parse_editions(table: &toml::Table) -> Result<std::collections::BTreeMap<String, Vec<String>>> {
    let mut editions = std::collections::BTreeMap::new();
    if let Some(t) = table.get("editions") {
        let t = t.as_table().context("[editions] is not a table")?;
        for key in t.keys() {
            let names = strings(t, key)?;
            if let Some(unknown) = names
                .iter()
                .find(|n| crate::build::Edition::from_name(n).is_none())
            {
                bail!("[editions] names the unknown edition `{unknown}` for {key}");
            }
            editions.insert(key.clone(), names);
        }
    }
    Ok(editions)
}

/// Fails if `table`, which `what` names, has a key other than `keys`.
fn only(table: &toml::Table, what: &str, keys: &[&str]) -> Result<()> {
    match table.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(key) => bail!("{what} has an unknown key `{key}`"),
        None => Ok(()),
    }
}

fn string(table: &toml::Table, key: &str) -> Result<String> {
    table
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .with_context(|| format!("no string `{key}`"))
}

/// The array `key` of `table`, empty when it has none.
fn array<'t>(table: &'t toml::Table, key: &str) -> Result<&'t [toml::Value]> {
    match table.get(key) {
        None => Ok(&[]),
        Some(v) => Ok(v
            .as_array()
            .with_context(|| format!("`{key}` is not an array"))?),
    }
}

fn strings(table: &toml::Table, key: &str) -> Result<Vec<String>> {
    array(table, key)?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .with_context(|| format!("`{key}` holds a value that is not a string"))
        })
        .collect()
}

/// Where a port was built.
pub struct Built {
    /// Its patched source.
    pub source: PathBuf,
    /// Its build directory, which holds its tests.
    pub build: PathBuf,
    /// The directory its prefix, `/system`, was installed in.
    pub install: PathBuf,
}

/// Where the tests of `port` find its source on Mikros.
pub fn test_source(port: &Port) -> String {
    format!("/system/share/{}-tests", port.name)
}

/// Where the programs of `port`'s build directory are on Mikros.
pub fn test_programs(port: &Port) -> String {
    format!("/system/libexec/{}-tests", port.name)
}

/// The port `name` of the file text `text`, for the tests of other
/// modules.
#[cfg(test)]
pub fn parse_for_tests(name: &str, text: &str) -> Port {
    parse(name, PathBuf::new(), text).expect("a port.toml that reads")
}

/// The prebuilt libraries among the ports in the folder `dir`, by name.
fn prebuilt_in(dir: &std::path::Path) -> Result<Vec<Port>> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("port.toml").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let mut found = Vec::new();
    for name in names {
        let port_dir = dir.join(&name);
        let path = port_dir.join("port.toml");
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let port =
            parse(&name, port_dir, &text).with_context(|| format!("in {}", path.display()))?;
        if port.prebuilt.is_some() {
            found.push(port);
        }
    }
    Ok(found)
}

/// Every prebuilt library that `user/ports` pins (MIKSPEC-0062.R18).
pub fn prebuilt_ports() -> Result<Vec<Port>> {
    prebuilt_in(&crate::workspace_root().join("user/ports"))
}

/// Where the archive of the prebuilt library `port` lies once fetched:
/// below `target/sources`, under the file name of its address.
pub fn prebuilt_archive(port: &Port) -> Result<PathBuf> {
    let file = port
        .url
        .split(['?', '#'])
        .next()
        .and_then(|u| u.rsplit('/').next())
        .filter(|f| !f.is_empty() && !f.starts_with('.'))
        .with_context(|| format!("the url of {} names no file", port.name))?;
    Ok(crate::workspace_root()
        .join("target/sources")
        .join(&port.name)
        .join(file))
}

/// Fetches the archive of the prebuilt library `port` and checks its
/// SHA-256, so that a mismatch fails before any file of it is used, and
/// returns where it lies. It is not downloaded again while the file there
/// has that checksum, so that a build after `cargo xtask setup` needs no
/// network (MIKSPEC-0062.R18).
pub fn fetch_prebuilt(port: &Port) -> Result<PathBuf> {
    let archive = prebuilt_archive(port)?;
    let what = format!("{} prebuilt library", port.name);
    crate::fetch::cached(&what, &port.url, &port.sha256, &archive)?;
    Ok(archive)
}

/// Fetches the source archive of `port`, a `.tar.gz`, unpacks it and
/// applies its patches, and returns the directory of the source.
pub fn source(port: &Port) -> Result<PathBuf> {
    if port.prebuilt.is_some() {
        bail!("{} is a prebuilt library, which has no source", port.name);
    }
    let root = crate::workspace_root();
    let dir = root.join("target/sources").join(&port.name);
    let archive = dir.join(format!("{}.tar.gz", port.source));
    let what = format!("{} source", port.name);
    crate::fetch::cached(&what, &port.url, &port.sha256, &archive)?;
    let patches = port.dir.join("patches");
    crate::port::patched_source(&archive, &dir, &port.source, &patches, &[])
}

/// Fetches, patches and builds `port`, a Meson project.
pub fn build(port: &Port) -> Result<Built> {
    let source = source(port)?;
    let install = crate::build_systems::meson(&port.name, &source, &port.options)?;
    Ok(Built {
        source,
        build: crate::build_systems::build_dir(&port.name),
        install,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xkbcommon_tests_read_their_data_from_their_source() {
        let xkbcommon = load("xkbcommon").unwrap();
        let option = format!("-Dtest-data-dir={}/test/data", test_source(&xkbcommon));
        assert!(xkbcommon.options.contains(&option));
    }

    #[test]
    fn a_port_names_the_editions_that_install_its_files() {
        use crate::build::Edition;
        let source = "[source]\nurl = \"u\"\nsha256 = \"s\"\ndir = \"d\"\n";
        let text = format!("{source}[editions]\n\"libraries/libx.dlib\" = [\"desktop\"]\n");
        let port = parse("x", PathBuf::new(), &text).unwrap();
        assert!(port.installed_in("libraries/libx.dlib", Edition::Desktop));
        assert!(!port.installed_in("libraries/libx.dlib", Edition::Server));
        assert!(port.installed_in("libraries/liby.dlib", Edition::Server));
        let unknown = format!("{source}[editions]\n\"a\" = [\"tablet\"]\n");
        assert!(parse("x", PathBuf::new(), &unknown).is_err());
    }

    #[test]
    fn a_table_that_names_no_file_of_the_port_fails() {
        let source = "[source]\nurl = \"u\"\nsha256 = \"s\"\ndir = \"d\"\n";
        let text = format!("{source}[editions]\n\"libraries/libx.dlib\" = [\"desktop\"]\n");
        let port = parse("x", PathBuf::new(), &text).unwrap();
        assert!(
            port.check_editions(&["libraries/libx.dlib", "libraries/liby.dlib"])
                .is_ok()
        );
        let error = port.check_editions(&["libraries/liby.dlib"]).unwrap_err();
        assert!(error.to_string().contains("libraries/libx.dlib"));
    }

    #[test]
    fn the_desktop_libraries_are_only_in_the_desktop_edition() {
        use crate::build::Edition;
        for (port, path) in [
            ("wayland", "libraries/libwayland.dlib"),
            ("vulkan-loader", "libraries/libvulkan.dlib"),
            ("mesa", "libraries/libvulkan_lvp.dlib"),
            ("mesa", "share/vulkan/icd.d/lvp_icd.x86_64.json"),
            ("llvm", "libraries/libLLVM.dlib"),
        ] {
            let port = load(port).unwrap();
            assert!(port.installed_in(path, Edition::Desktop), "{path}");
            assert!(!port.installed_in(path, Edition::Server), "{path}");
        }
    }

    #[test]
    fn only_the_ports_that_the_build_reads_have_an_editions_table() {
        let dir = crate::workspace_root().join("user/ports");
        for entry in fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            let port = load(&name).unwrap();
            if !EDITION_PORTS.contains(&name.as_str()) && port.prebuilt.is_none() {
                assert!(
                    port.editions.is_empty(),
                    "{name} has an [editions] table that the build never reads"
                );
            }
        }
    }

    #[test]
    fn every_port_reads() {
        let dir = crate::workspace_root().join("user/ports");
        for entry in fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            load(&name).unwrap();
        }
    }

    /// A `[prebuilt]` table with the keys of MIKSPEC-0062.R18.
    fn prebuilt_text(extra: &str) -> String {
        format!(
            "[prebuilt]\nurl = \"https://example.org/x.tar.zst\"\nsha256 = \"abc\"\n\
             repository = \"https://example.org/x\"\n\
             commit = \"0123456789abcdef0123456789abcdef01234567\"\n\
             workflow = \"https://example.org/runs/1\"\nsysroot = \"sysroot-1\"\n\
             toolchain = \"23.1.4\"\n{extra}"
        )
    }

    #[test]
    fn reads_a_prebuilt_library() {
        let port = parse("x", PathBuf::new(), &prebuilt_text("")).unwrap();
        let prebuilt = port.prebuilt.as_ref().unwrap();
        assert_eq!(port.url, "https://example.org/x.tar.zst");
        assert_eq!(port.sha256, "abc");
        assert_eq!(prebuilt.commit, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(prebuilt.sysroot, "sysroot-1");
        assert!(port.source.is_empty() && port.options.is_empty());
        // The editions that hold a file of a prebuilt library are named too.
        let text = prebuilt_text("[editions]\n\"libraries/libx.dlib\" = [\"desktop\"]\n");
        let port = parse("x", PathBuf::new(), &text).unwrap();
        assert!(!port.installed_in("libraries/libx.dlib", crate::build::Edition::Server));
    }

    #[test]
    fn refuses_a_prebuilt_library_that_lacks_a_key_or_has_one_empty() {
        for key in [
            "url",
            "sha256",
            "repository",
            "commit",
            "workflow",
            "sysroot",
            "toolchain",
        ] {
            let lines: Vec<String> = prebuilt_text("")
                .lines()
                .filter(|l| !l.starts_with(&format!("{key} =")))
                .map(str::to_owned)
                .collect();
            let missing = parse("x", PathBuf::new(), &lines.join("\n")).unwrap_err();
            assert!(format!("{missing:#}").contains(key), "{key}");
            let empty = prebuilt_text("").replace(
                prebuilt_text("")
                    .lines()
                    .find(|l| l.starts_with(&format!("{key} =")))
                    .unwrap(),
                &format!("{key} = \"\""),
            );
            let error = parse("x", PathBuf::new(), &empty).unwrap_err();
            assert!(format!("{error:#}").contains(key), "{key}");
        }
    }

    #[test]
    fn refuses_a_commit_that_is_not_40_lowercase_hex_digits() {
        let good = "0123456789abcdef0123456789abcdef01234567";
        for bad in [
            "0123456789abcdef0123456789abcdef0123456",
            "0123456789abcdef0123456789abcdef012345678",
            "0123456789ABCDEF0123456789abcdef01234567",
            "0123456789abcdef0123456789abcdef0123456g",
        ] {
            let text = prebuilt_text("").replace(good, bad);
            let error = parse("x", PathBuf::new(), &text).unwrap_err();
            assert!(format!("{error:#}").contains("40 lowercase"), "{bad}");
        }
    }

    #[test]
    fn a_prebuilt_library_has_no_source_build_or_tests() {
        for other in [
            "[source]\nurl = \"u\"\nsha256 = \"s\"\ndir = \"d\"\n",
            "[build]\noptions = []\n",
            "[tests]\ndata = []\n",
        ] {
            let error = parse("x", PathBuf::new(), &prebuilt_text(other)).unwrap_err();
            assert!(
                format!("{error:#}").contains("a prebuilt library has no"),
                "{other}"
            );
        }
        let port = parse("x", PathBuf::new(), &prebuilt_text("")).unwrap();
        assert!(source(&port).is_err());
    }

    #[test]
    fn finds_the_prebuilt_libraries_among_the_ports() {
        let dir = std::env::temp_dir().join(format!("xtask-ports-{}", std::process::id()));
        for (name, text) in [
            (
                "a-source",
                "[source]\nurl = \"u\"\nsha256 = \"s\"\ndir = \"d\"\n".to_owned(),
            ),
            ("b-prebuilt", prebuilt_text("")),
            ("c-folder-without-port", String::new()),
        ] {
            fs::create_dir_all(dir.join(name)).unwrap();
            if name != "c-folder-without-port" {
                fs::write(dir.join(name).join("port.toml"), text).unwrap();
            }
        }
        let found = prebuilt_in(&dir).unwrap();
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["b-prebuilt"]);
        // A port that does not parse is an error, not skipped.
        fs::write(dir.join("a-source/port.toml"), "[prebuilt]\n").unwrap();
        assert!(prebuilt_in(&dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_archive_of_a_prebuilt_library_lies_under_the_file_name_of_its_address() {
        let port = parse("x", PathBuf::new(), &prebuilt_text("")).unwrap();
        let archive = prebuilt_archive(&port).unwrap();
        assert!(
            archive.ends_with("target/sources/x/x.tar.zst"),
            "{archive:?}"
        );
        let mut other = parse("x", PathBuf::new(), &prebuilt_text("")).unwrap();
        other.url = "https://example.org/a/b.tar.gz?download=1#top".to_owned();
        assert!(
            prebuilt_archive(&other)
                .unwrap()
                .ends_with("target/sources/x/b.tar.gz")
        );
        other.url = "https://example.org/".to_owned();
        assert!(prebuilt_archive(&other).is_err());
    }

    #[test]
    fn rejects_unknown_keys() {
        let ok = "[source]\nurl = \"u\"\nsha256 = \"s\"\ndir = \"d\"\n";
        assert!(parse("x", PathBuf::new(), ok).is_ok());
        let typo = "[source]\nurl = \"u\"\nsha265 = \"s\"\ndir = \"d\"\n";
        assert!(parse("x", PathBuf::new(), typo).is_err());
        let crates = format!("{ok}[[crate]]\nname = \"a\"\n");
        assert!(parse("x", PathBuf::new(), &crates).is_err());
    }
}
