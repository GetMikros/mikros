//! The components of user space that the system image holds, as their
//! packages in the `user` workspaces declare them in
//! `[package.metadata.mikros]`:
//!
//! - `kind`: where the program goes. A `core` component or a `program`
//!   goes to `usr/bin/<name>`, a program with its manifest in
//!   `usr/lib/programs/`; a `service` goes to `usr/bin/<name>` with its
//!   manifest in `usr/lib/services/`; a `driver` goes to
//!   `usr/lib/drivers/<name>/<name>` with its `driver.yaml`.
//! - `image`: `boot` for the boot image, which the system image holds
//!   as well, or `system` for the system image only.
//! - `manifest`: the file name of a service's or program's manifest in
//!   its package. A `<stem>.diag.yaml` beside it replaces it in the
//!   diagnostic build.
//! - `runtime`: `rt` for a program built on `mikros-rt` without the
//!   standard library, whose programs are the default.
//! - `kinds`: the build kinds that install it, when not all of them.
//! - `path`: the member name of the program instead, for `init`.

use crate::build::Kind;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspaces whose packages can be components, relative to the
/// repository.
pub const WORKSPACES: [&str; 2] = ["user", "user/runtime/std"];

/// What a component is, which says where it is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Core,
    Service,
    Driver,
    Program,
}

/// The image that holds a component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Image {
    /// The boot image, and the system image with it.
    Boot,
    /// Only the system image.
    System,
}

/// The library a program is built on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runtime {
    /// `mikros-rt`, without the standard library.
    Rt,
    /// The standard library.
    Std,
}

/// A component: a program of the package of the same name.
#[derive(Debug)]
pub struct Component {
    pub name: String,
    /// The package's directory.
    pub dir: PathBuf,
    pub role: Role,
    pub image: Image,
    pub runtime: Runtime,
    manifest: Option<String>,
    kinds: Option<Vec<String>>,
    path: Option<String>,
}

impl Component {
    /// Whether the build kind `kind` installs the component.
    pub fn in_kind(&self, kind: Kind) -> bool {
        self.kinds
            .as_ref()
            .is_none_or(|kinds| kinds.iter().any(|k| k == kind.name()))
    }

    /// The members that install the component in an image of the build
    /// kind `kind`, with its program built at `program`: the program and
    /// its manifest, each by its name in the image.
    pub fn members(&self, kind: Kind, program: &Path) -> Result<Vec<(String, PathBuf)>> {
        let name = &self.name;
        let at = match (&self.path, self.role) {
            (Some(path), _) => path.clone(),
            (None, Role::Driver) => format!("usr/lib/drivers/{name}/{name}"),
            (None, _) => format!("usr/bin/{name}"),
        };
        let mut members = vec![(at, program.to_path_buf())];
        let manifest = match (self.role, &self.manifest) {
            (Role::Driver, _) => Some((
                format!("usr/lib/drivers/{name}/driver.yaml"),
                self.dir.join("driver.yaml"),
            )),
            (Role::Service, Some(file)) => Some((
                format!("usr/lib/services/{file}"),
                self.manifest_source(kind, file),
            )),
            (Role::Program, Some(file)) => Some((
                format!("usr/lib/programs/{file}"),
                self.manifest_source(kind, file),
            )),
            (Role::Service, None) => bail!("the service {name} names no manifest"),
            (Role::Core | Role::Program, None) => None,
            (Role::Core, Some(_)) => bail!("the core component {name} has a manifest"),
        };
        members.extend(manifest);
        Ok(members)
    }

    /// The file of the manifest `file` for the build kind `kind`: the
    /// diagnostic build's variant when it has one.
    fn manifest_source(&self, kind: Kind, file: &str) -> PathBuf {
        let diag = self
            .dir
            .join(file.replace(".yaml", &format!(".{}.yaml", Kind::Diag.name())));
        if kind == Kind::Diag && diag.is_file() {
            diag
        } else {
            self.dir.join(file)
        }
    }
}

/// Every component, in the order of `cargo metadata`, which is by name.
pub fn all() -> Result<Vec<Component>> {
    let mut components = Vec::new();
    for workspace in WORKSPACES {
        components.extend(parse(&metadata(workspace)?)?);
    }
    Ok(components)
}

/// The names of the programs of every package of the workspace
/// `workspace`, relative to the repository.
pub fn programs(workspace: &str) -> Result<Vec<String>> {
    let metadata = metadata(workspace)?;
    let mut programs = Vec::new();
    for package in metadata["packages"].as_array().context("no packages")? {
        for target in package["targets"].as_array().context("no targets")? {
            let kinds = target["kind"]
                .as_array()
                .context("a target without kinds")?;
            if kinds.iter().any(|k| k == "bin") {
                let name = target["name"].as_str().context("a target without a name")?;
                programs.push(name.to_owned());
            }
        }
    }
    Ok(programs)
}

/// The output of `cargo metadata` for the workspace `workspace`,
/// relative to the repository, without dependencies.
fn metadata(workspace: &str) -> Result<Value> {
    let manifest = crate::workspace_root().join(workspace).join("Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .arg("--manifest-path")
        .arg(&manifest)
        .output()
        .context("running cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata for {} failed: {}",
            manifest.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// The components among the packages of `cargo metadata`'s output.
fn parse(metadata: &Value) -> Result<Vec<Component>> {
    let mut components = Vec::new();
    let packages = metadata["packages"].as_array().context("no packages")?;
    for package in packages {
        let name = package["name"]
            .as_str()
            .context("a package without a name")?;
        let Some(table) = package["metadata"]["mikros"].as_object() else {
            continue;
        };
        let text = |key: &str| -> Result<Option<String>> {
            match table.get(key) {
                None => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => bail!("{name}: `{key}` is not a string"),
            }
        };
        for key in table.keys() {
            if !["kind", "image", "manifest", "runtime", "kinds", "path"].contains(&key.as_str()) {
                bail!("{name}: unknown key `{key}` in [package.metadata.mikros]");
            }
        }
        let runtime = match text("runtime")?.as_deref() {
            None => Runtime::Std,
            Some("rt") => Runtime::Rt,
            Some(other) => bail!("{name}: unknown runtime `{other}`"),
        };
        let role = match text("kind")?.as_deref() {
            None => continue,
            Some("core") => Role::Core,
            Some("service") => Role::Service,
            Some("driver") => Role::Driver,
            Some("program") => Role::Program,
            Some(other) => bail!("{name}: unknown kind `{other}`"),
        };
        let image = match text("image")?.as_deref() {
            Some("boot") => Image::Boot,
            Some("system") => Image::System,
            Some(other) => bail!("{name}: unknown image `{other}`"),
            None => bail!("{name}: a component names its image"),
        };
        let kinds = match table.get("kinds") {
            None => None,
            Some(Value::Array(kinds)) => Some(
                kinds
                    .iter()
                    .map(|k| k.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
                    .with_context(|| format!("{name}: `kinds` holds a non-string"))?,
            ),
            Some(_) => bail!("{name}: `kinds` is not a list"),
        };
        let manifest_path = package["manifest_path"]
            .as_str()
            .context("a package without a manifest path")?;
        components.push(Component {
            name: name.to_owned(),
            dir: Path::new(manifest_path)
                .parent()
                .context("a manifest path without a directory")?
                .to_path_buf(),
            role,
            image,
            runtime,
            manifest: text("manifest")?,
            kinds,
            path: text("path")?,
        });
    }
    Ok(components)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str, mikros: Value) -> Value {
        serde_json::json!({
            "name": name,
            "manifest_path": format!("/r/user/x/{name}/Cargo.toml"),
            "metadata": { "mikros": mikros },
        })
    }

    #[test]
    fn installs_each_role_where_it_belongs() {
        let metadata = serde_json::json!({ "packages": [
            package("nvme", serde_json::json!({"kind": "driver", "image": "boot", "runtime": "rt"})),
            package("logsvc", serde_json::json!({"kind": "service", "image": "boot", "runtime": "rt", "manifest": "log.yaml"})),
            package("init", serde_json::json!({"kind": "core", "image": "boot", "runtime": "rt", "path": "init"})),
            package("msh", serde_json::json!({"kind": "program", "image": "system"})),
            { "name": "mikros-yaml", "manifest_path": "/r/user/lib/yaml/Cargo.toml", "metadata": null },
        ]});
        let components = parse(&metadata).unwrap();
        assert_eq!(components.len(), 4);
        let members = |i: usize| -> Vec<String> {
            components[i]
                .members(Kind::Debug, Path::new("/bin"))
                .unwrap()
                .into_iter()
                .map(|(at, _)| at)
                .collect()
        };
        assert_eq!(
            members(0),
            [
                "usr/lib/drivers/nvme/nvme",
                "usr/lib/drivers/nvme/driver.yaml"
            ]
        );
        assert_eq!(members(1), ["usr/bin/logsvc", "usr/lib/services/log.yaml"]);
        assert_eq!(members(2), ["init"]);
        assert_eq!(members(3), ["usr/bin/msh"]);
        assert_eq!(components[3].runtime, Runtime::Std);
        assert_eq!(components[0].runtime, Runtime::Rt);
    }

    #[test]
    fn rejects_unknown_keys_and_values() {
        for mikros in [
            serde_json::json!({"kind": "driver", "image": "boot", "imgae": "x"}),
            serde_json::json!({"kind": "daemon", "image": "boot"}),
            serde_json::json!({"kind": "driver"}),
            serde_json::json!({"kind": "driver", "image": "boot", "runtime": "none"}),
        ] {
            let metadata = serde_json::json!({ "packages": [package("x", mikros)] });
            assert!(parse(&metadata).is_err());
        }
    }

    #[test]
    fn takes_only_the_kinds_it_names() {
        let metadata = serde_json::json!({ "packages": [package("diagsvc",
            serde_json::json!({"kind": "service", "image": "system", "manifest": "diag.yaml", "kinds": ["diag"]}))]});
        let components = parse(&metadata).unwrap();
        assert!(components[0].in_kind(Kind::Diag));
        assert!(!components[0].in_kind(Kind::Production));
    }
}
