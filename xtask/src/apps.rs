//! The system apps that packages of the `user` workspace are: a package
//! whose `[package.metadata.mikros]` has `kind = "app"`, in `user/apps/<name>/`
//! (MIKSPEC-0023.R62). They are found by that table and installed as the
//! files of an app package at `/system/apps/system.mikros.<name>/`, with the
//! manifest that xtask writes (MIKSPEC-0101.R77, MIKSPEC-0101.R82).

use crate::build::Edition;
use crate::standin::Part;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// The roles that a shell part provides (MIKSPEC-0075.R23).
const ROLES: [&str; 9] = [
    "menubar",
    "taskbar",
    "start-menu",
    "desktop",
    "switcher",
    "file-picker",
    "capture-picker",
    "greeter",
    "prompts",
];

/// The keys that the table of an app may have.
const KEYS: [&str; 6] = [
    "kind",
    "name",
    "description",
    "shell_parts",
    "files",
    "editions",
];

/// The longest part of an ID, in bytes (MIKSPEC-0101.R78).
const NAME_MAX: usize = 114;

/// A system app that a package of `user/apps/` is.
#[derive(Debug)]
pub struct App {
    /// The package's name, which is the app's name in its ID and the
    /// path of its program.
    pub package: String,
    pub version: String,
    /// The `name` of the table.
    pub name: String,
    pub description: String,
    pub parts: Vec<Part>,
    /// Each file by its path in the app, its path on the host and whether
    /// it is executable.
    pub files: Vec<(String, PathBuf, bool)>,
    editions: Option<Vec<String>>,
}

impl App {
    /// The ID of the app.
    pub fn id(&self) -> String {
        format!("system.mikros.{}", self.package)
    }

    /// Whether the edition `edition` installs the app.
    pub fn in_edition(&self, edition: Edition) -> bool {
        self.editions
            .as_ref()
            .is_none_or(|editions| editions.iter().any(|e| e == edition.name()))
    }

    /// The members that install the app in a system image, with its
    /// program built at `program`, each by its name in the image: its
    /// files, which the manifest below `dir` has the checksum of, and the
    /// manifest.
    pub fn members(&self, program: &Path, dir: &Path) -> Result<Vec<(String, PathBuf)>> {
        let mut files = vec![(self.package.clone(), program.to_path_buf(), true)];
        files.extend(self.files.iter().cloned());
        let id = self.id();
        let parts = crate::standin_build::shell_parts(&self.parts, None)
            .with_context(|| format!("the shell parts of {}", self.package))?;
        crate::system_apps::members(
            &crate::system_apps::SystemApp {
                id: &id,
                version: &self.version,
                name: &self.name,
                description: &self.description,
                executable: &self.package,
                portable: false,
                installable: false,
                shell_parts: &parts,
                files,
            },
            dir,
        )
    }
}

/// The apps of the `user` workspace.
pub fn all() -> Result<Vec<App>> {
    parse(&crate::components::metadata()?)
}

/// Fails, naming the package, for an app that is not one and for an app
/// that has the ID of one that a stand-in adds (MIKSPEC-0023.R62).
pub fn check_all() -> Result<()> {
    let stand_ins = crate::standin::find(&crate::workspace_root().join("user"))?;
    check_ids(
        &all()?,
        stand_ins
            .iter()
            .flat_map(|s| s.apps.iter().map(|a| a.id.as_str())),
    )
}

/// Fails when an app has the ID of an app that a stand-in adds
/// (MIKSPEC-0023.R62).
pub fn check_ids<'a>(apps: &[App], stand_in_ids: impl IntoIterator<Item = &'a str>) -> Result<()> {
    for id in stand_in_ids {
        if let Some(app) = apps.iter().find(|a| a.id() == id) {
            bail!(
                "the app {} and a stand-in have the same ID, {id}",
                app.package
            );
        }
    }
    Ok(())
}

/// Whether `name` is one part of an ID (MIKSPEC-0101.R78).
fn is_part(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= NAME_MAX
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The apps among the packages of `cargo metadata`'s output.
fn parse(metadata: &Value) -> Result<Vec<App>> {
    let mut apps = Vec::new();
    for package in metadata["packages"].as_array().context("no packages")? {
        let Some(table) = package["metadata"]["mikros"].as_object() else {
            continue;
        };
        if table.get("kind").and_then(Value::as_str) != Some("app") {
            continue;
        }
        let name = package["name"]
            .as_str()
            .context("a package without a name")?;
        apps.push(parse_app(package, table, name).with_context(|| format!("the app {name}"))?);
    }
    apps.sort_by(|a, b| a.package.cmp(&b.package));
    Ok(apps)
}

fn strings(value: &Value, what: &str) -> Result<Vec<String>> {
    value
        .as_array()
        .with_context(|| format!("{what} is not a list"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .with_context(|| format!("{what} holds a value that is not a string"))
        })
        .collect()
}

/// The items of the list `key` of `table`; none when it is missing.
fn list<'t>(table: &'t serde_json::Map<String, Value>, key: &str) -> Result<&'t [Value]> {
    match table.get(key) {
        None => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        Some(_) => bail!("`{key}` is not a list"),
    }
}

fn parse_app(package: &Value, table: &serde_json::Map<String, Value>, name: &str) -> Result<App> {
    if let Some(key) = table.keys().find(|k| !KEYS.contains(&k.as_str())) {
        bail!("unknown key `{key}` in [package.metadata.mikros]");
    }
    let dir = Path::new(package["manifest_path"].as_str().unwrap_or(""))
        .parent()
        .context("a package without a manifest")?
        .to_path_buf();
    let folder = |p: &Path| p.file_name().and_then(|n| n.to_str()).map(str::to_owned);
    let in_apps = dir.parent().and_then(folder).as_deref() == Some("apps")
        && dir
            .parent()
            .and_then(Path::parent)
            .and_then(folder)
            .as_deref()
            == Some("user");
    if !in_apps || folder(&dir).as_deref() != Some(name) {
        bail!("its folder must be user/apps/{name}");
    }
    if !is_part(name) {
        bail!("its name is not one part of an ID: lowercase letters, digits and `_`");
    }
    let text = |key: &str| -> Result<String> {
        match table.get(key) {
            Some(Value::String(s)) => Ok(s.clone()),
            Some(_) => bail!("`{key}` is not a string"),
            None => bail!("no `{key}` in [package.metadata.mikros]"),
        }
    };
    let title = text("name")?;
    let description = text("description")?;
    let builds_program = package["targets"].as_array().is_some_and(|targets| {
        targets.iter().any(|t| {
            t["name"].as_str() == Some(name)
                && t["kind"]
                    .as_array()
                    .is_some_and(|k| k.iter().any(|k| k.as_str() == Some("bin")))
        })
    });
    if !builds_program {
        bail!("it builds no program named {name}");
    }
    let mut files = Vec::new();
    for file in list(table, "files")? {
        let file = file.as_object().context("`files` holds a non-table")?;
        for key in file.keys() {
            if !["name", "from", "executable"].contains(&key.as_str()) {
                bail!("unknown key `{key}` in a file of `files`");
            }
        }
        let at = file
            .get("name")
            .and_then(Value::as_str)
            .context("a file without a `name`")?;
        let from = file
            .get("from")
            .and_then(Value::as_str)
            .context("a file without `from`")?;
        let executable = file
            .get("executable")
            .and_then(Value::as_bool)
            .context("a file without `executable`")?;
        if at == name || at == "manifest.yaml" {
            bail!("a file is named {at}, which the app has already");
        }
        for path in [at, from] {
            let plain = Path::new(path)
                .components()
                .all(|c| matches!(c, Component::Normal(_)));
            if path.is_empty() || !plain {
                bail!("the path {path:?} of a file is not a relative path inside the folder");
            }
        }
        let host = dir.join(from);
        if !host.is_file() {
            bail!("the file {from} is missing in {}", dir.display());
        }
        files.push((at.to_owned(), host, executable));
    }
    let mut parts = Vec::new();
    for part in list(table, "shell_parts")? {
        let part = part
            .as_object()
            .context("`shell_parts` holds a non-table")?;
        for key in part.keys() {
            if !["roles", "executable", "arguments"].contains(&key.as_str()) {
                bail!("unknown key `{key}` in a shell part");
            }
        }
        let roles = strings(part.get("roles").unwrap_or(&Value::Null), "`roles`")
            .context("a shell part has no roles")?;
        if roles.is_empty() {
            bail!("a shell part has no role");
        }
        if let Some(role) = roles.iter().find(|r| !ROLES.contains(&r.as_str())) {
            bail!("a shell part has the role {role}, which is none");
        }
        let executable = part
            .get("executable")
            .and_then(Value::as_str)
            .context("a shell part without an `executable`")?
            .to_owned();
        if executable != name && !files.iter().any(|(at, _, _)| *at == executable) {
            bail!("the executable {executable} of a shell part is neither the program nor a file");
        }
        let arguments = match part.get("arguments") {
            None => Vec::new(),
            Some(a) => strings(a, "`arguments`")?,
        };
        parts.push(Part {
            roles,
            executable,
            arguments,
        });
    }
    let editions = match table.get("editions") {
        None => None,
        Some(e) => {
            let editions = strings(e, "`editions`")?;
            if let Some(edition) = editions.iter().find(|e| Edition::from_name(e).is_none()) {
                bail!("unknown edition `{edition}`");
            }
            Some(editions)
        }
    };
    // The shell components are not in the server edition (MIKSPEC-0023.R53).
    if !parts.is_empty()
        && editions
            .as_ref()
            .is_none_or(|e| e.iter().any(|e| e == "server"))
    {
        bail!("an app with shell parts names its `editions`, without `server`");
    }
    Ok(App {
        package: name.to_owned(),
        version: package["version"]
            .as_str()
            .context("a package without a version")?
            .to_owned(),
        name: title,
        description,
        parts,
        files,
        editions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `cargo metadata` output with one package at `dir`, whose table
    /// is `table`.
    fn metadata(dir: &str, table: Value) -> Value {
        let name = dir.rsplit('/').next().unwrap();
        json!({"packages": [{
            "name": name,
            "version": "1.2.3",
            "manifest_path": format!("{dir}/Cargo.toml"),
            "targets": [{"name": name, "kind": ["bin"]}],
            "metadata": {"mikros": table},
        }]})
    }

    fn table() -> Value {
        json!({"kind": "app", "name": "Hello", "description": "Says hello"})
    }

    fn only(dir: &str, table: Value) -> Result<App> {
        Ok(parse(&metadata(dir, table))?.remove(0))
    }

    fn fails(dir: &str, table: Value) -> String {
        format!("{:#}", only(dir, table).unwrap_err())
    }

    #[test]
    fn reads_the_table_of_an_app() {
        let app = only("/r/user/apps/hello", table()).unwrap();
        assert_eq!(app.id(), "system.mikros.hello");
        assert_eq!(app.version, "1.2.3");
        assert_eq!(app.name, "Hello");
        assert!(app.parts.is_empty() && app.files.is_empty());
        assert!(app.in_edition(Edition::Server) && app.in_edition(Edition::Desktop));
    }

    #[test]
    fn leaves_other_packages_alone() {
        let metadata = metadata("/r/user/bin/x", json!({"kind": "program"}));
        assert!(parse(&metadata).unwrap().is_empty());
    }

    #[test]
    fn refuses_a_folder_or_a_name_that_is_not_an_id_part() {
        assert!(fails("/r/user/bin/hello", table()).contains("user/apps/hello"));
        assert!(fails("/r/apps/hello", table()).contains("user/apps/hello"));
        for bad in ["Hello", "he-llo", "he.llo", "1x", "_x"] {
            let dir = format!("/r/user/apps/{bad}");
            assert!(fails(&dir, table()).contains("one part"), "{bad}");
        }
        let long = format!("a{}", "b".repeat(NAME_MAX));
        assert!(fails(&format!("/r/user/apps/{long}"), table()).contains("one part"));
        let longest = format!("a{}", "b".repeat(NAME_MAX - 1));
        assert!(only(&format!("/r/user/apps/{longest}"), table()).is_ok());
    }

    #[test]
    fn refuses_unknown_keys_and_a_missing_name_or_description() {
        let mut t = table();
        t["image"] = json!("system");
        assert!(fails("/r/user/apps/hello", t).contains("unknown key `image`"));
        assert!(
            fails(
                "/r/user/apps/hello",
                json!({"kind": "app", "description": "d"})
            )
            .contains("no `name`")
        );
        assert!(
            fails("/r/user/apps/hello", json!({"kind": "app", "name": "n"}))
                .contains("no `description`")
        );
    }

    #[test]
    fn refuses_a_package_without_its_program() {
        let mut metadata = metadata("/r/user/apps/hello", table());
        metadata["packages"][0]["targets"] = json!([{"name": "hello", "kind": ["lib"]}]);
        let error = format!("{:#}", parse(&metadata).unwrap_err());
        assert!(error.contains("builds no program"), "{error}");
    }

    #[test]
    fn checks_the_shell_parts() {
        let part = |roles: Value, executable: &str| {
            let mut t = table();
            t["editions"] = json!(["desktop"]);
            t["shell_parts"] = json!([{"roles": roles, "executable": executable}]);
            t
        };
        let ok = only("/r/user/apps/hello", part(json!(["greeter"]), "hello")).unwrap();
        assert_eq!(ok.parts[0].roles, ["greeter"]);
        assert!(fails("/r/user/apps/hello", part(json!([]), "hello")).contains("no role"));
        assert!(
            fails("/r/user/apps/hello", part(json!(["dock"]), "hello")).contains("which is none")
        );
        assert!(
            fails("/r/user/apps/hello", part(json!(["greeter"]), "other"))
                .contains("neither the program nor a file")
        );
    }

    #[test]
    fn an_app_with_shell_parts_leaves_out_the_server_edition() {
        let mut t = table();
        t["shell_parts"] = json!([{"roles": ["greeter"], "executable": "hello"}]);
        assert!(fails("/r/user/apps/hello", t.clone()).contains("without `server`"));
        t["editions"] = json!(["server", "desktop"]);
        assert!(fails("/r/user/apps/hello", t.clone()).contains("without `server`"));
        t["editions"] = json!(["desktop"]);
        let app = only("/r/user/apps/hello", t).unwrap();
        assert!(app.in_edition(Edition::Desktop) && !app.in_edition(Edition::Server));
    }

    #[test]
    fn refuses_an_unknown_edition() {
        let mut t = table();
        t["editions"] = json!(["tablet"]);
        assert!(fails("/r/user/apps/hello", t).contains("unknown edition `tablet`"));
    }

    #[test]
    fn checks_the_files() {
        let dir =
            std::env::temp_dir().join(format!("xtask-apps-{}/user/apps/hello", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("data.txt"), "x").unwrap();
        let path = dir.to_str().unwrap();
        let files = |files: Value| {
            let mut t = table();
            t["files"] = files;
            t
        };
        let ok = only(
            path,
            files(json!([{"name": "share/data", "from": "data.txt", "executable": false}])),
        )
        .unwrap();
        assert_eq!(ok.files[0].0, "share/data");
        for (file, what) in [
            (
                json!({"name": "hello", "from": "data.txt", "executable": false}),
                "already",
            ),
            (
                json!({"name": "manifest.yaml", "from": "data.txt", "executable": false}),
                "already",
            ),
            (
                json!({"name": "a", "from": "missing.txt", "executable": false}),
                "missing",
            ),
            (
                json!({"name": "a", "from": "../x", "executable": false}),
                "relative path",
            ),
            (
                json!({"name": "/a", "from": "data.txt", "executable": false}),
                "relative path",
            ),
            (json!({"name": "a", "from": "data.txt"}), "`executable`"),
        ] {
            let error = format!("{:#}", only(path, files(json!([file]))).unwrap_err());
            assert!(error.contains(what), "{error}");
        }
        std::fs::remove_dir_all(dir.parent().unwrap().parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn an_app_and_a_stand_in_do_not_share_an_id() {
        let apps = parse(&metadata("/r/user/apps/hello", table())).unwrap();
        assert!(check_ids(&apps, ["system.mikros.other"]).is_ok());
        let error = check_ids(&apps, ["system.mikros.hello"]).unwrap_err();
        assert!(error.to_string().contains("same ID"));
    }

    #[test]
    fn installs_the_program_and_the_manifest_with_the_checksum() {
        let program =
            std::env::temp_dir().join(format!("xtask-apps-program-{}", std::process::id()));
        std::fs::write(&program, "program").unwrap();
        let out = std::env::temp_dir().join(format!("xtask-apps-out-{}", std::process::id()));
        let app = only("/r/user/apps/hello", table()).unwrap();
        let members = app.members(&program, &out).unwrap();
        let names: Vec<&str> = members.iter().map(|(at, _)| at.as_str()).collect();
        assert_eq!(
            names,
            [
                "system/apps/system.mikros.hello/hello",
                "system/apps/system.mikros.hello/manifest.yaml"
            ]
        );
        let manifest = std::fs::read_to_string(&members[1].1).unwrap();
        assert!(manifest.contains("id: system.mikros.hello\nkind: app\nversion: 1.2.3\n"));
        assert!(manifest.contains("executable: hello\nportable: false\ninstallable: false\n"));
        assert!(manifest.contains("checksum: "));
        let _ = std::fs::remove_file(program);
        let _ = std::fs::remove_dir_all(out);
    }
}
