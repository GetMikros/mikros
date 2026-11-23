//! The stand-ins that a folder describes with a file `stand-in.toml`
//! (MIKSPEC-0023.R59): components that an image holds only because another
//! component is not implemented yet. This module finds the files, reads them
//! and checks everything that can be checked without building: the names
//! that `cargo xtask` resolves later are the ports, headers and protocol
//! origins below.

// Nothing builds from these descriptions yet; the next commits do.
#![allow(dead_code)]

use crate::build::Edition;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The file that describes a stand-in.
pub const FILE: &str = "stand-in.toml";

/// The names of the header folders that `cargo xtask` resolves.
pub const INCLUDES: [&str; 3] = ["vulkan-headers", "sdl3", "generated-protocols"];

/// The names of the ports whose outputs a program can link.
pub const PORTS: [&str; 4] = ["wayland", "vulkan-loader", "sdl3", "xkbcommon"];

/// How a program links an output of a port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Linking {
    Dynamic,
    Static,
}

/// An output of a port that a program links: the port and its file in the
/// port's libraries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub port: String,
    pub file: String,
    pub linking: Linking,
}

/// Where a Wayland protocol's XML file comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The protocols that `cargo xtask` stages from its ports, as
    /// `<name>.xml`.
    Staged,
    /// The folder `protocols` of the component of the repository with this
    /// name, as `<name>.xml`.
    Component(String),
    /// The file at this path in the sysroot.
    Sysroot(String),
}

/// A Wayland protocol whose client header and private code are generated and
/// compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Protocol {
    pub name: String,
    pub origin: Origin,
}

/// The C program that a stand-in builds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    pub output: String,
    /// The C sources in the folder.
    pub sources: Vec<String>,
    pub optimization: String,
    pub warnings: Vec<String>,
    pub includes: Vec<String>,
    pub link: Vec<Link>,
    pub protocols: Vec<Protocol>,
}

/// One part of the manifest's `shell_parts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub roles: Vec<String>,
    pub executable: String,
    /// The arguments, in which `{password}` stands for the account's.
    pub arguments: Vec<String>,
}

/// A file of a system app's package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppFile {
    /// Its path in the package.
    pub name: String,
    /// `self` for the program of the stand-in, or the name of a stand-in
    /// that it uses, whose program it is.
    pub from: String,
    pub executable: bool,
}

/// A system app that a stand-in adds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub executable: String,
    pub portable: bool,
    pub installable: bool,
    pub parts: Vec<Part>,
    pub files: Vec<AppFile>,
}

/// A file of the folder that is installed as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Install {
    /// Its path under `/system`.
    pub path: String,
    /// Its path in the folder.
    pub file: String,
}

/// The account that a stand-in adds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub full_name: String,
    pub kind: String,
    pub login: String,
}

/// A stand-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StandIn {
    /// Its folder.
    pub dir: PathBuf,
    pub name: String,
    /// The requirement that says why it is a stand-in.
    pub reason: String,
    pub editions: Vec<Edition>,
    pub uses: Vec<String>,
    pub enables_desktop: bool,
    pub build: Option<Build>,
    pub apps: Vec<App>,
    pub installs: Vec<Install>,
    pub account: Option<Account>,
}

impl StandIn {
    /// Whether an image of the edition `edition` holds it.
    pub fn in_edition(&self, edition: Edition) -> bool {
        self.editions.contains(&edition)
    }
}

fn only(table: &toml::Table, what: &str, keys: &[&str]) -> Result<()> {
    match table.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(key) => bail!("{what} has an unknown key `{key}`"),
        None => Ok(()),
    }
}

fn string(table: &toml::Table, what: &str, key: &str) -> Result<String> {
    table
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .with_context(|| format!("{what} has no string `{key}`"))
}

fn boolean(table: &toml::Table, what: &str, key: &str) -> Result<bool> {
    table
        .get(key)
        .and_then(|v| v.as_bool())
        .with_context(|| format!("{what} has no boolean `{key}`"))
}

fn strings(table: &toml::Table, what: &str, key: &str) -> Result<Vec<String>> {
    table
        .get(key)
        .and_then(|v| v.as_array())
        .with_context(|| format!("{what} has no list `{key}`"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .with_context(|| format!("`{key}` of {what} holds a value that is not a string"))
        })
        .collect()
}

/// The tables of the array `key` of `table`; none when it is missing.
fn tables<'t>(table: &'t toml::Table, what: &str, key: &str) -> Result<Vec<&'t toml::Table>> {
    match table.get(key) {
        None => Ok(Vec::new()),
        Some(value) => value
            .as_array()
            .with_context(|| format!("`{key}` of {what} is not an array of tables"))?
            .iter()
            .map(|v| {
                v.as_table()
                    .with_context(|| format!("`{key}` of {what} holds a value that is no table"))
            })
            .collect(),
    }
}

/// Whether `id` is a requirement ID such as `MIKSPEC-0075.R14`.
fn is_requirement(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("MIKSPEC-") else {
        return false;
    };
    let Some((document, requirement)) = rest.split_once(".R") else {
        return false;
    };
    document.len() == 4
        && document.bytes().all(|b| b.is_ascii_digit())
        && !requirement.is_empty()
        && requirement.bytes().all(|b| b.is_ascii_digit())
}

fn parse_origin(from: &str, table: &toml::Table, what: &str) -> Result<Origin> {
    if from == "staged" {
        only(table, what, &["name", "from"])?;
        Ok(Origin::Staged)
    } else if from == "sysroot" {
        only(table, what, &["name", "from", "path"])?;
        Ok(Origin::Sysroot(string(table, what, "path")?))
    } else if let Some(component) = from.strip_prefix("component:") {
        only(table, what, &["name", "from"])?;
        if component.is_empty() {
            bail!("{what} names no component");
        }
        Ok(Origin::Component(component.to_owned()))
    } else {
        bail!("{what} comes from `{from}`, which is not staged, a component or the sysroot")
    }
}

fn parse_build(table: &toml::Table, dir: &Path, name: &str) -> Result<Build> {
    let what = format!("[build] of {name}");
    only(
        table,
        &what,
        &[
            "output",
            "sources",
            "optimization",
            "warnings",
            "includes",
            "link",
            "protocols",
        ],
    )?;
    let sources = strings(table, &what, "sources")?;
    if sources.is_empty() {
        bail!("{what} has no sources");
    }
    for source in &sources {
        if !source.ends_with(".c") || source.contains("..") || source.starts_with('/') {
            bail!("{what}: `{source}` is not a C source in the folder");
        }
        if !dir.join(source).is_file() {
            bail!("{what}: the source {source} is not in {}", dir.display());
        }
    }
    let optimization = string(table, &what, "optimization")?;
    if !optimization.starts_with("-O") {
        bail!("{what}: `{optimization}` is not an optimization option");
    }
    let warnings = strings(table, &what, "warnings")?;
    if let Some(bad) = warnings.iter().find(|w| !w.starts_with("-W")) {
        bail!("{what}: `{bad}` is not a warning option");
    }
    let includes = strings(table, &what, "includes")?;
    if let Some(bad) = includes.iter().find(|i| !INCLUDES.contains(&i.as_str())) {
        bail!("{what}: `{bad}` is not a header folder that cargo xtask resolves");
    }
    let mut link = Vec::new();
    for t in tables(table, &what, "link")? {
        only(t, &what, &["port", "file", "linking"])?;
        let port = string(t, &what, "port")?;
        if !PORTS.contains(&port.as_str()) {
            bail!("{what}: `{port}` is not a port that cargo xtask links");
        }
        let file = string(t, &what, "file")?;
        let linking = match string(t, &what, "linking")?.as_str() {
            "dynamic" => Linking::Dynamic,
            "static" => Linking::Static,
            other => bail!("{what}: `{other}` is neither dynamic nor static"),
        };
        let suffix = match linking {
            Linking::Dynamic => ".dlib",
            Linking::Static => ".slib",
        };
        if !file.ends_with(suffix) || file.contains('/') {
            bail!("{what}: {file} cannot be linked {linking:?}ally");
        }
        link.push(Link {
            port,
            file,
            linking,
        });
    }
    let mut protocols: Vec<Protocol> = Vec::new();
    for t in tables(table, &what, "protocols")? {
        let protocol = string(t, &what, "name")?;
        let from = string(t, &what, "from")?;
        let origin = parse_origin(&from, t, &format!("the protocol {protocol} of {what}"))?;
        if protocols.iter().any(|p| p.name == protocol) {
            bail!("{what} lists the protocol {protocol} twice");
        }
        protocols.push(Protocol {
            name: protocol,
            origin,
        });
    }
    if !protocols.is_empty() && !includes.iter().any(|i| i == "generated-protocols") {
        bail!("{what} generates protocol code but does not include its folder");
    }
    Ok(Build {
        output: string(table, &what, "output")?,
        sources,
        optimization,
        warnings,
        includes,
        link,
        protocols,
    })
}

fn parse_app(table: &toml::Table, name: &str) -> Result<App> {
    let what = format!("a system app of {name}");
    only(
        table,
        &what,
        &[
            "id",
            "kind",
            "version",
            "name",
            "description",
            "executable",
            "portable",
            "installable",
            "shell_parts",
            "files",
        ],
    )?;
    let id = string(table, &what, "id")?;
    if !id.starts_with("system.mikros.") || !mikros_mika::id::is_valid(&id) {
        bail!("{id}, a system app of {name}, is not an ID of a system app");
    }
    if string(table, &what, "kind")? != "app" {
        bail!("{id} is a system app whose kind is not `app`");
    }
    let mut parts = Vec::new();
    for t in tables(table, &id, "shell_parts")? {
        only(t, &id, &["roles", "executable", "arguments"])?;
        parts.push(Part {
            roles: strings(t, &id, "roles")?,
            executable: string(t, &id, "executable")?,
            arguments: strings(t, &id, "arguments")?,
        });
    }
    if parts.iter().any(|p| p.roles.is_empty()) {
        bail!("{id} has a shell part without a role");
    }
    let mut files = Vec::new();
    for t in tables(table, &id, "files")? {
        only(t, &id, &["name", "from", "executable"])?;
        files.push(AppFile {
            name: string(t, &id, "name")?,
            from: string(t, &id, "from")?,
            executable: boolean(t, &id, "executable")?,
        });
    }
    if files.is_empty() {
        bail!("{id} has no files");
    }
    let executable = string(table, &what, "executable")?;
    if !files.iter().any(|f| f.name == executable) {
        bail!("{id} does not hold its executable {executable}");
    }
    for part in &parts {
        if !files.iter().any(|f| f.name == part.executable) {
            bail!(
                "{id} does not hold {}, a program of its shell parts",
                part.executable
            );
        }
    }
    Ok(App {
        id,
        version: string(table, &what, "version")?,
        name: string(table, &what, "name")?,
        description: string(table, &what, "description")?,
        executable,
        portable: boolean(table, &what, "portable")?,
        installable: boolean(table, &what, "installable")?,
        parts,
        files,
    })
}

/// The stand-in that `text`, the file `stand-in.toml` of the folder `dir`,
/// describes.
pub fn parse(text: &str, dir: &Path) -> Result<StandIn> {
    let table: toml::Table = text.parse().context("not TOML")?;
    only(
        &table,
        "the file",
        &["stand-in", "build", "system_apps", "installs", "account"],
    )?;
    let head = table
        .get("stand-in")
        .and_then(|t| t.as_table())
        .context("no [stand-in] table")?;
    only(
        head,
        "[stand-in]",
        &[
            "stand_in",
            "name",
            "reason",
            "editions",
            "uses",
            "enables_desktop",
        ],
    )?;
    if !boolean(head, "[stand-in]", "stand_in")? {
        bail!("[stand-in] says that this is not a stand-in");
    }
    let name = string(head, "[stand-in]", "name")?;
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        bail!("`{name}` is not a name of a stand-in");
    }
    let reason = string(head, "[stand-in]", "reason")?;
    if !is_requirement(&reason) {
        bail!("{name}: `{reason}` is not a requirement ID");
    }
    let editions = strings(head, "[stand-in]", "editions")?
        .iter()
        .map(|e| Edition::from_name(e).with_context(|| format!("{name}: no edition is named {e}")))
        .collect::<Result<Vec<_>>>()?;
    if editions.is_empty() {
        bail!("{name} has no editions");
    }
    let uses = strings(head, "[stand-in]", "uses")?;
    let enables_desktop = boolean(head, "[stand-in]", "enables_desktop")?;
    let build = match table.get("build") {
        None => None,
        Some(t) => Some(parse_build(
            t.as_table().context("[build] is not a table")?,
            dir,
            &name,
        )?),
    };
    let apps = tables(&table, &name, "system_apps")?
        .into_iter()
        .map(|t| parse_app(t, &name))
        .collect::<Result<Vec<_>>>()?;
    for app in &apps {
        for file in &app.files {
            let known = file.from == "self" && build.is_some() || uses.contains(&file.from);
            if !known {
                bail!(
                    "{} takes {} from `{}`, which is neither this stand-in's program nor one it uses",
                    app.id,
                    file.name,
                    file.from
                );
            }
        }
    }
    let mut installs = Vec::new();
    for t in tables(&table, &name, "installs")? {
        only(t, &name, &["path", "file"])?;
        let install = Install {
            path: string(t, &name, "path")?,
            file: string(t, &name, "file")?,
        };
        if install.path.starts_with('/') || install.path.contains("..") {
            bail!("{name}: {} is not a path under /system", install.path);
        }
        if install.file.contains("..") || !dir.join(&install.file).is_file() {
            bail!(
                "{name}: the file {} to install is not in the folder",
                install.file
            );
        }
        installs.push(install);
    }
    let account = match table.get("account") {
        None => None,
        Some(t) => {
            let t = t.as_table().context("[account] is not a table")?;
            only(t, "[account]", &["name", "full_name", "kind", "login"])?;
            let account = Account {
                name: string(t, "[account]", "name")?,
                full_name: string(t, "[account]", "full_name")?,
                kind: string(t, "[account]", "kind")?,
                login: string(t, "[account]", "login")?,
            };
            if !["admin", "standard"].contains(&account.kind.as_str()) {
                bail!("{name}: the kind of the account is not admin or standard");
            }
            if !["console", "desktop"].contains(&account.login.as_str()) {
                bail!("{name}: the login mode of the account is not console or desktop");
            }
            Some(account)
        }
    };
    Ok(StandIn {
        dir: dir.to_owned(),
        name,
        reason,
        editions,
        uses,
        enables_desktop,
        build,
        apps,
        installs,
        account,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirement_ids_have_a_document_and_a_number() {
        assert!(is_requirement("MIKSPEC-0075.R14"));
        assert!(!is_requirement("MIKSPEC-0075"));
        assert!(!is_requirement("MIKSPEC-75.R1"));
        assert!(!is_requirement("MIKSPEC-0075.R"));
        assert!(!is_requirement("RFC-0075.R1"));
    }
}
