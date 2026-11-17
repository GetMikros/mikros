//! A package's `manifest.yaml` (MIKSPEC-0101.R3). `parse` reads every key
//! that is there; `check` then requires what a package that is installed
//! or run needs, which a manifest that `pkg pack` fills in
//! (MIKSPEC-0101.R12) or that a build folder holds (MIKSPEC-0101.R10)
//! may lack.

use crate::format::{self, MANIFEST};
use crate::id;
use crate::version::{Range, Version};
use alloc::string::String;
use alloc::vec::Vec;
use mikros_yaml::{Node, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    App,
    Library,
    Driver,
}

/// A command of the package (MIKSPEC-0101.R17).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bin {
    pub name: String,
    pub path: String,
}

/// A package inside the package, required unless it is optional
/// (MIKSPEC-0101.R62).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    pub path: String,
    pub optional: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub kind: Option<Kind>,
    pub version: Option<Version>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub screenshots: Vec<String>,
    pub recordings: Vec<String>,
    pub checksum: Option<String>,
    pub signature: Option<String>,
    pub executable: Option<String>,
    pub portable: bool,
    pub installable: bool,
    /// Whether the package is installed for every user, as `install`
    /// says, which nothing changes (MIKSPEC-0101.R62).
    pub global: bool,
    pub needs: Vec<(String, Range)>,
    pub components: Vec<Component>,
    pub bins: Vec<Bin>,
    pub services: Vec<(String, String)>,
    pub driver: Option<String>,
}

/// What is wrong with a manifest: the key, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub key: &'static str,
    pub why: &'static str,
}

fn error<T>(key: &'static str, why: &'static str) -> Result<T, Error> {
    Err(Error { key, why })
}

/// Whether `name` is a command's name: lowercase ASCII letters, digits,
/// `.`, `_`, `+` and `-`, starting with a letter or digit.
pub fn is_command(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '+' | '-')
        })
}

/// Whether `path` is a path in the package: relative, with no empty, `.`
/// or `..` component.
pub fn is_package_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

fn string(node: &Node, key: &'static str) -> Result<String, Error> {
    match node.string() {
        Ok(s) => Ok(String::from(s)),
        Err(_) => error(key, "is not a string"),
    }
}

fn path(node: &Node, key: &'static str) -> Result<String, Error> {
    let path = string(node, key)?;
    match is_package_path(&path) {
        true => Ok(path),
        false => error(key, "is not a relative path in the package"),
    }
}

fn paths(node: &Node, key: &'static str) -> Result<Vec<String>, Error> {
    let Ok(list) = node.list() else {
        return error(key, "is not a list");
    };
    list.iter().map(|n| path(n, key)).collect()
}

fn boolean(node: Option<&Node>, key: &'static str) -> Result<bool, Error> {
    match node {
        None => Ok(true),
        Some(n) => n.boolean().or(error(key, "is not true or false")),
    }
}

impl Manifest {
    /// The manifest in `bytes`, with an `id` that is a package ID and the
    /// other keys it has in their forms.
    pub fn parse(bytes: &[u8]) -> Result<Manifest, Error> {
        let Ok(root) = mikros_yaml::parse(bytes) else {
            return error("manifest", "is not YAML with a map at its root");
        };
        let Ok(map) = root.map() else {
            return error("manifest", "is not a map");
        };
        let id = match map.get("id") {
            Some(n) => string(n, "id")?,
            None => return error("id", "is missing"),
        };
        if !id::is_valid(&id) {
            return error("id", "is not a package ID");
        }
        let kind = match map
            .get("kind")
            .map(|n| string(n, "kind"))
            .transpose()?
            .as_deref()
        {
            None => None,
            Some("app") => Some(Kind::App),
            Some("library") => Some(Kind::Library),
            Some("driver") => Some(Kind::Driver),
            Some(_) => return error("kind", "is not app, library or driver"),
        };
        let version = match map.get("version") {
            None => None,
            Some(n) => match Version::parse(&string(n, "version")?) {
                Some(v) => Some(v),
                None => return error("version", "is not a version"),
            },
        };
        let text = |key: &'static str| map.get(key).map(|n| string(n, key)).transpose();
        let one_path = |key: &'static str| map.get(key).map(|n| path(n, key)).transpose();
        let list = |key: &'static str| match map.get(key) {
            None => Ok(Vec::new()),
            Some(n) => paths(n, key),
        };
        let checksum = text("checksum")?;
        if let Some(sum) = &checksum {
            let hex = sum.strip_prefix("sha256:").unwrap_or("");
            let lower = hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if hex.len() != 64 || !lower {
                return error(
                    "checksum",
                    "is not sha256: and 64 lowercase hexadecimal digits",
                );
            }
        }
        let global = match text("install")?.as_deref() {
            None | Some("global") => true,
            Some("local") => false,
            Some(_) => return error("install", "is not global or local"),
        };
        if kind == Some(Kind::Driver) && !global {
            return error("install", "is local for a driver");
        }
        let mut needs = Vec::new();
        if let Some(n) = map.get("needs") {
            let Value::Map(entries) = &n.value else {
                return error("needs", "is not a map");
            };
            for entry in entries {
                if !id::is_valid(&entry.key) {
                    return error("needs", "names something that is not a package ID");
                }
                let range = string(&entry.value, "needs")?;
                let Some(range) = Range::parse(&range) else {
                    return error("needs", "holds something that is not a version range");
                };
                needs.push((entry.key.clone(), range));
            }
        }
        let mut components = Vec::new();
        if let Some(n) = map.get("components") {
            let Ok(list) = n.list() else {
                return error("components", "is not a list");
            };
            for item in list {
                let component = match item.map() {
                    Err(_) => Component {
                        path: path(item, "components")?,
                        optional: false,
                    },
                    Ok(entry) => {
                        let Some(at) = entry.get("path") else {
                            return error("components", "holds a map without a path");
                        };
                        let optional = match entry.get("optional") {
                            None => false,
                            Some(n) => n.boolean().or(error(
                                "components",
                                "has optional that is not true or false",
                            ))?,
                        };
                        Component {
                            path: path(at, "components")?,
                            optional,
                        }
                    }
                };
                components.push(component);
            }
        }
        let mut bins = Vec::new();
        if let Some(n) = map.get("bins") {
            let Ok(list) = n.list() else {
                return error("bins", "is not a list");
            };
            for item in list {
                let Ok(bin) = item.map() else {
                    return error("bins", "holds something that is not a map");
                };
                let (Some(name), Some(at)) = (bin.get("name"), bin.get("path")) else {
                    return error("bins", "holds a command without name or path");
                };
                let name = string(name, "bins")?;
                if !is_command(&name) {
                    return error("bins", "names a command that is not a command's name");
                }
                bins.push(Bin {
                    name,
                    path: path(at, "bins")?,
                });
            }
        }
        let mut services = Vec::new();
        if let Some(n) = map.get("services") {
            let Value::Map(entries) = &n.value else {
                return error("services", "is not a map");
            };
            for entry in entries {
                services.push((entry.key.clone(), path(&entry.value, "services")?));
            }
        }
        Ok(Manifest {
            id,
            kind,
            version,
            name: text("name")?,
            description: text("description")?,
            icon: one_path("icon")?,
            screenshots: list("screenshots")?,
            recordings: list("recordings")?,
            checksum,
            signature: one_path("signature")?,
            executable: one_path("executable")?,
            portable: boolean(map.get("portable"), "portable")?,
            installable: boolean(map.get("installable"), "installable")?,
            global,
            needs,
            components,
            bins,
            services,
            driver: one_path("driver")?,
        })
    }

    /// Checks that the manifest has what every package needs: a kind, a
    /// version, a name, a description and a checksum, an executable for
    /// an app and a driver manifest for a driver, and no ID of the system.
    pub fn check(&self) -> Result<(), Error> {
        let Some(kind) = self.kind else {
            return error("kind", "is missing");
        };
        if self.version.is_none() {
            return error("version", "is missing");
        }
        if self.name.is_none() {
            return error("name", "is missing");
        }
        if self.description.is_none() {
            return error("description", "is missing");
        }
        if self.checksum.is_none() {
            return error("checksum", "is missing");
        }
        if kind == Kind::App && self.executable.is_none() {
            return error("executable", "is missing");
        }
        if kind == Kind::Driver && self.driver.is_none() {
            return error("driver", "is missing");
        }
        if id::is_system(&self.id) {
            return error("id", "is one of the system's");
        }
        Ok(())
    }

    /// Checks the manifest of the package `package` against it: its
    /// checksum, and that the files it names are there.
    pub fn check_package(&self, package: &format::Package) -> Result<(), Error> {
        self.check()?;
        let checksum = format::checksum_text(&package.checksum(self.signature.as_deref()));
        if self.checksum.as_deref() != Some(checksum.as_str()) {
            return error("checksum", "does not match the package");
        }
        let file = |path: &str| {
            package
                .find(path)
                .is_some_and(|e| e.kind != format::Kind::Folder)
        };
        let named = self
            .icon
            .iter()
            .chain(&self.screenshots)
            .chain(&self.recordings)
            .chain(&self.signature)
            .chain(&self.executable)
            .chain(self.components.iter().map(|c| &c.path))
            .chain(self.bins.iter().map(|b| &b.path))
            .chain(self.services.iter().map(|s| &s.1))
            .chain(&self.driver);
        for path in named {
            if path == MANIFEST || !file(path) {
                return error("manifest", "names a file that the package does not hold");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
