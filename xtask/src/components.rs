//! The components of user space that the system image holds, as their
//! packages in the `user` workspaces declare them in
//! `[package.metadata.mikros]`:
//!
//! - `kind`: where the program goes. A `core` component goes to
//!   `system/<name>`; a `program` goes to `system/bin/<name>`, with its
//!   manifest in `system/programs/`; a `service` goes to
//!   `system/services/system.<name>/<name>` with its manifest
//!   `service.yaml` there; a `driver` goes to
//!   `system/drivers/system.driver.<name>/<name>` with its `driver.yaml`;
//!   a `test`, a system test, which only test images hold, goes to
//!   `system/bin/tests/<name>`.
//! - `image`: `boot` for the boot image, which the system image holds
//!   as well, `system` for the system image only, or `tests` for test
//!   images only.
//! - `manifest`: the file name of a service's or program's manifest in
//!   its package. A `<stem>.diag.yaml` beside it replaces it in the
//!   diagnostic build.
//! - `runtime`: `rt` for a program built on `mikros-rt` without the
//!   standard library, whose programs are the default; `std` for a
//!   library that needs the standard library, which only programs with
//!   it use.
//! - `kinds`: the build kinds that install it, when not all of them.
//! - `path`: the member name of the program instead, for `init`.
//! - `completions`: the file name, in its package, of the declarations of
//!   a program's arguments for msh, installed as
//!   `system/share/msh/completions/<name>.msh`.
//! - `config`: the file names, in its package, of settings of the system
//!   that it installs as `config/<file>`, in the root file system's
//!   subvolume `config`, which processes see as `/system/config`.
//! - `share`: the paths, in its package, of data files that it installs
//!   as `system/share/<path>`.
//! - `cursors`: the folders, in its package, of cursor themes, each of
//!   which `cursors::build` turns into `system/share/cursors/<folder>.curpack`.

use crate::build::Kind;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What a component is, which says where it is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Core,
    Service,
    Driver,
    Program,
    Test,
}

/// The image that holds a component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Image {
    /// The boot image, and the system image with it.
    Boot,
    /// Only the system image.
    System,
    /// Only test images.
    Tests,
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
    completions: Option<String>,
    config: Vec<String>,
    share: Vec<String>,
    cursors: Vec<String>,
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
            (None, Role::Driver) => format!("{}/{name}", driver_dir(name)),
            (None, Role::Service) => format!("{}/{name}", service_dir(name)),
            (None, Role::Core) => format!("system/{name}"),
            (None, Role::Test) => format!("system/bin/tests/{name}"),
            (None, Role::Program) => format!("system/bin/{name}"),
        };
        let mut members = vec![(at, program.to_path_buf())];
        let manifest = match (self.role, &self.manifest) {
            (Role::Driver, _) => Some((
                format!("{}/driver.yaml", driver_dir(name)),
                self.dir.join("driver.yaml"),
            )),
            (Role::Service, Some(file)) => Some((
                format!("{}/service.yaml", service_dir(name)),
                self.manifest_source(kind, file),
            )),
            (Role::Program, Some(file)) => Some((
                format!("system/programs/{file}"),
                self.manifest_source(kind, file),
            )),
            (Role::Service, None) => bail!("the service {name} names no manifest"),
            (Role::Core | Role::Program | Role::Test, None) => None,
            (Role::Core | Role::Test, Some(_)) => bail!("{name} cannot have a manifest"),
        };
        members.extend(manifest);
        if let Some(file) = &self.completions {
            members.push((
                format!("system/share/msh/completions/{name}.msh"),
                self.dir.join(file),
            ));
        }
        for file in &self.config {
            members.push((format!("config/{file}"), self.dir.join(file)));
        }
        for path in &self.share {
            members.push((format!("system/share/{path}"), self.dir.join(path)));
        }
        for folder in &self.cursors {
            let pack = crate::cursors::build(&self.dir.join(folder))?;
            let name = pack.file_name().context("a cursor theme without a name")?;
            let name = name.to_string_lossy();
            members.push((format!("system/share/cursors/{name}"), pack));
        }
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

/// The folder in an image of the service `name`, named by its app ID.
pub fn service_dir(name: &str) -> String {
    format!("system/services/system.{name}")
}

/// The folder in an image of the system's driver `name`, named by its app
/// ID.
pub fn driver_dir(name: &str) -> String {
    format!("system/drivers/system.driver.{name}")
}

/// Every component, in the order of `cargo metadata`, which is by name.
pub fn all() -> Result<Vec<Component>> {
    parse(&metadata()?)
}

/// The programs of every package of the `user` workspace, by name, with
/// the runtime each is built on.
/// The libraries that declare `runtime = "std"`, by package name.
pub fn std_libraries() -> Result<Vec<String>> {
    let metadata = metadata()?;
    let mut libraries = Vec::new();
    for package in metadata["packages"].as_array().context("no packages")? {
        if package["metadata"]["mikros"]["runtime"].as_str() == Some("std") {
            let name = package["name"]
                .as_str()
                .context("a package without a name")?;
            libraries.push(name.to_owned());
        }
    }
    Ok(libraries)
}

pub fn programs() -> Result<Vec<(String, Runtime)>> {
    let metadata = metadata()?;
    let mut programs = Vec::new();
    for package in metadata["packages"].as_array().context("no packages")? {
        let runtime = runtime(package)?;
        for target in package["targets"].as_array().context("no targets")? {
            let kinds = target["kind"]
                .as_array()
                .context("a target without kinds")?;
            if kinds.iter().any(|k| k == "bin") {
                let name = target["name"].as_str().context("a target without a name")?;
                programs.push((name.to_owned(), runtime));
            }
        }
    }
    Ok(programs)
}

/// The runtime that the package `package` of `cargo metadata`'s output
/// declares, or the standard library.
fn runtime(package: &Value) -> Result<Runtime> {
    let name = package["name"].as_str().unwrap_or("?");
    match &package["metadata"]["mikros"]["runtime"] {
        Value::Null => Ok(Runtime::Std),
        Value::String(s) if s == "rt" => Ok(Runtime::Rt),
        Value::String(s) if s == "std" => Ok(Runtime::Std),
        other => bail!("{name}: unknown runtime {other}"),
    }
}

/// The output of `cargo metadata` for the `user` workspace, without
/// dependencies.
fn metadata() -> Result<Value> {
    let manifest = crate::workspace_root().join("user/Cargo.toml");
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
            let keys = [
                "kind",
                "image",
                "manifest",
                "runtime",
                "kinds",
                "path",
                "completions",
                "config",
                "share",
                "cursors",
            ];
            if !keys.contains(&key.as_str()) {
                bail!("{name}: unknown key `{key}` in [package.metadata.mikros]");
            }
        }
        let runtime = runtime(package)?;
        let role = match text("kind")?.as_deref() {
            None => continue,
            Some("core") => Role::Core,
            Some("service") => Role::Service,
            Some("driver") => Role::Driver,
            Some("program") => Role::Program,
            Some("test") => Role::Test,
            Some(other) => bail!("{name}: unknown kind `{other}`"),
        };
        let image = match text("image")?.as_deref() {
            Some("boot") => Image::Boot,
            Some("system") => Image::System,
            Some("tests") => Image::Tests,
            Some(other) => bail!("{name}: unknown image `{other}`"),
            None => bail!("{name}: a component names its image"),
        };
        if role == Role::Test && image != Image::Tests {
            bail!("{name}: a test is in test images only, `image = \"tests\"`");
        }
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
        let list = |key: &str| -> Result<Vec<String>> {
            match table.get(key) {
                None => Ok(Vec::new()),
                Some(Value::Array(files)) => files
                    .iter()
                    .map(|f| f.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
                    .with_context(|| format!("{name}: `{key}` holds a non-string")),
                Some(_) => bail!("{name}: `{key}` is not a list"),
            }
        };
        let (config, share, cursors) = (list("config")?, list("share")?, list("cursors")?);
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
            completions: text("completions")?,
            config,
            share,
            cursors,
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
            package("logsvc", serde_json::json!({"kind": "service", "image": "boot", "runtime": "rt", "manifest": "logsvc.yaml"})),
            package("init", serde_json::json!({"kind": "core", "image": "boot", "runtime": "rt", "path": "init"})),
            package("msh", serde_json::json!({"kind": "program", "image": "system"})),
            { "name": "mikros-yaml", "manifest_path": "/r/user/lib/yaml/Cargo.toml", "metadata": null },
            package("devices", serde_json::json!({"kind": "program", "image": "system", "completions": "devices.msh"})),
            package("sessionmgr", serde_json::json!({"kind": "service", "image": "system", "runtime": "rt", "manifest": "s.yaml", "config": ["session.yaml"]})),
            package("compositor", serde_json::json!({"kind": "program", "image": "system", "share": ["desktop/shell.yaml"]})),
        ]});
        let components = parse(&metadata).unwrap();
        assert_eq!(components.len(), 7);
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
                "system/drivers/system.driver.nvme/nvme",
                "system/drivers/system.driver.nvme/driver.yaml"
            ]
        );
        assert_eq!(
            members(1),
            [
                "system/services/system.logsvc/logsvc",
                "system/services/system.logsvc/service.yaml"
            ]
        );
        assert_eq!(members(2), ["init"]);
        assert_eq!(members(3), ["system/bin/msh"]);
        assert_eq!(
            members(4),
            [
                "system/bin/devices",
                "system/share/msh/completions/devices.msh"
            ]
        );
        assert_eq!(
            members(5),
            [
                "system/services/system.sessionmgr/sessionmgr",
                "system/services/system.sessionmgr/service.yaml",
                "config/session.yaml"
            ]
        );
        assert_eq!(
            members(6),
            ["system/bin/compositor", "system/share/desktop/shell.yaml"]
        );
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
    fn installs_system_tests_in_test_images_only() {
        let metadata = serde_json::json!({ "packages": [package("files",
            serde_json::json!({"kind": "test", "image": "tests"}))]});
        let components = parse(&metadata).unwrap();
        let members = components[0]
            .members(Kind::Debug, Path::new("/bin"))
            .unwrap();
        assert_eq!(members[0].0, "system/bin/tests/files");
        assert_eq!(members.len(), 1);
        let elsewhere = serde_json::json!({ "packages": [package("files",
            serde_json::json!({"kind": "test", "image": "system"}))]});
        assert!(parse(&elsewhere).is_err());
    }

    #[test]
    fn takes_only_the_kinds_it_names() {
        let metadata = serde_json::json!({ "packages": [package("diagsvc",
            serde_json::json!({"kind": "service", "image": "system", "manifest": "diagsvc.yaml", "kinds": ["diag"]}))]});
        let components = parse(&metadata).unwrap();
        assert!(components[0].in_kind(Kind::Diag));
        assert!(!components[0].in_kind(Kind::Production));
    }
}
