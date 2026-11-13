//! The third-party projects of user space, each pinned in
//! `user/ports/<name>/port.toml` with its Mikros changes beside it:
//!
//! - `[source]`: the `url` of the archive of its source, the archive's
//!   `sha256`, the directory `dir` that holds the source once it is
//!   unpacked, and `rewrites`, pairs of texts: the first of each is
//!   replaced with the second in every file of the source when it is
//!   unpacked;
//! - `[[crate]]`: for a Rust project, each crate it depends on that needs
//!   changes too, by `name`, `version` and `sha256`, and whether the
//!   rewrites apply to it, `rewritten`;
//! - `[build]`: the `options` that a Meson project is configured with;
//! - `[tests]`: `data`, the directories of its source that its tests read.
//!
//! The changes that are not rewrites are patch files, in `patches/`. The
//! Meson projects are built here, the others by modules of their own.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::PathBuf;

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
    pub rewrites: Vec<(String, String)>,
    pub crates: Vec<Crate>,
    /// The options it is configured with.
    pub options: Vec<String>,
    /// The directories of its source, relative to it, that its tests
    /// read.
    pub test_data: Vec<String>,
}

/// A crate that a ported Rust project depends on, which needs changes.
pub struct Crate {
    pub name: String,
    pub version: String,
    pub sha256: String,
    /// Whether the port's rewrites apply to it.
    pub rewritten: bool,
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
    only(&table, "the file", &["source", "crate", "build", "tests"])?;
    let source = table
        .get("source")
        .and_then(|s| s.as_table())
        .context("no [source] table")?;
    only(source, "[source]", &["url", "sha256", "dir", "rewrites"])?;
    let mut rewrites = Vec::new();
    for pair in array(source, "rewrites")? {
        match pair.as_array().map(|p| p.as_slice()) {
            Some([toml::Value::String(from), toml::Value::String(to)]) => {
                rewrites.push((from.clone(), to.clone()));
            }
            _ => bail!("a rewrite is not a pair of strings"),
        }
    }
    let mut crates = Vec::new();
    if let Some(list) = table.get("crate") {
        for c in list
            .as_array()
            .context("`crate` is not an array of tables")?
        {
            let c = c.as_table().context("a [[crate]] is not a table")?;
            only(c, "[[crate]]", &["name", "version", "sha256", "rewritten"])?;
            crates.push(Crate {
                name: string(c, "name")?,
                version: string(c, "version")?,
                sha256: string(c, "sha256")?,
                rewritten: c
                    .get("rewritten")
                    .and_then(|r| r.as_bool())
                    .context("a [[crate]] does not say whether it is `rewritten`")?,
            });
        }
    }
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
        rewrites,
        crates,
        options: section("build", &["options"])?,
        test_data: section("tests", &["data"])?,
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
    /// The directory its prefix, `/usr`, was installed in.
    pub install: PathBuf,
}

/// Where the tests of `port` find its source on Mikros.
pub fn test_source(port: &Port) -> String {
    format!("/usr/share/{}-tests", port.name)
}

/// Where the programs of `port`'s build directory are on Mikros.
pub fn test_programs(port: &Port) -> String {
    format!("/usr/libexec/{}-tests", port.name)
}

/// Fetches, patches and builds `port`, a Meson project.
pub fn build(port: &Port) -> Result<Built> {
    let root = crate::workspace_root();
    let dir = root.join("target/sources").join(&port.name);
    let archive = dir.join(format!("{}.tar.gz", port.source));
    let what = format!("{} source", port.name);
    crate::fetch::cached(&what, &port.url, &port.sha256, &archive)?;
    let patches = port.dir.join("patches");
    let source = crate::port::patched_source(&archive, &dir, &port.source, &patches, &[])?;
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
        let rewrite = format!("{ok}rewrites = [[\"a\"]]\n");
        assert!(parse("x", PathBuf::new(), &rewrite).is_err());
    }
}
