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
//! Its changes are patch files, in `patches/`. The
//! Meson projects are built here, the others by modules of their own.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::PathBuf;

/// The ports whose `[editions]` tables the build reads, because it takes
/// files from them for the system image.
pub const EDITION_PORTS: [&str; 4] = ["wayland", "vulkan-loader", "mesa", "llvm"];

/// A ported project.
pub struct Port {
    /// Its name, which names its directories.
    pub name: String,
    /// Its directory in `user/ports`.
    pub dir: PathBuf,
    pub url: String,
    /// SHA-256 of the archive at `url`.
    pub sha256: String,
    /// The directory that holds the source once it is unpacked.
    pub source: String,
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

fn parse(name: &str, dir: PathBuf, text: &str) -> Result<Port> {
    let table: toml::Table = text.parse()?;
    only(
        &table,
        "the file",
        &["source", "build", "tests", "editions"],
    )?;
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
    Ok(Port {
        name: name.to_owned(),
        dir,
        url: string(source, "url")?,
        sha256: string(source, "sha256")?,
        source: string(source, "dir")?,
        options: section("build", &["options"])?,
        test_data: section("tests", &["data"])?,
        editions,
    })
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

/// Fetches the source archive of `port`, a `.tar.gz`, unpacks it and
/// applies its patches, and returns the directory of the source.
pub fn source(port: &Port) -> Result<PathBuf> {
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
            if !EDITION_PORTS.contains(&name.as_str()) {
                assert!(
                    load(&name).unwrap().editions.is_empty(),
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
