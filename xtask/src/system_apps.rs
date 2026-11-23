//! The base copies of system apps: the files of an app package laid out
//! as installed, at `system/apps/<id>/` of a system image, with the
//! `manifest.yaml` that xtask writes for them, whose `checksum` is the
//! content checksum of those files (MIKSPEC-0101.R77, MIKSPEC-0101.R2).

use anyhow::{Context, Result, anyhow, bail};
use std::fs;
use std::path::{Path, PathBuf};

/// The start of the ID of every system app.
const PREFIX: &str = "system.mikros.";

/// A system app: the keys of its manifest, and its files.
pub struct SystemApp<'a> {
    pub id: &'a str,
    pub version: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    /// The path, in the package, of the program that runs the app.
    pub executable: &'a str,
    pub portable: bool,
    pub installable: bool,
    /// The manifest's `shell_parts`: the lines of its list, indented.
    pub shell_parts: &'a str,
    /// Each file by its path in the package, with whether it is
    /// executable.
    pub files: Vec<(String, PathBuf, bool)>,
}

/// The text of `app`'s manifest, with `checksum`.
fn manifest(app: &SystemApp, checksum: &str) -> Result<String> {
    for text in [app.name, app.description] {
        if text.contains(['"', '\\', '\n']) {
            bail!("the name or description of {} needs escaping", app.id);
        }
    }
    let text = format!(
        "id: {}\nkind: app\nversion: {}\nname: \"{}\"\ndescription: \"{}\"\n\
         checksum: {checksum}\nexecutable: {}\nportable: {}\ninstallable: {}\n\
         shell_parts:\n{}",
        app.id,
        app.version,
        app.name,
        app.description,
        app.executable,
        app.portable,
        app.installable,
        app.shell_parts
    );
    mikros_mika::manifest::Manifest::parse(text.as_bytes())
        .map_err(|e| anyhow!("the manifest of {} is invalid: {e:?}", app.id))?;
    Ok(text)
}

/// The members that install `app` in a system image, each by its name in
/// the image: its files and its manifest, which is written below `dir`.
pub fn members(app: &SystemApp, dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    if !app.id.starts_with(PREFIX) || !mikros_mika::id::is_valid(app.id) {
        bail!("{} is not the ID of a system app", app.id);
    }
    let mut package = mikros_mika::format::Builder::default();
    for (path, file, executable) in &app.files {
        let data = fs::read(file).with_context(|| format!("reading {}", file.display()))?;
        package.file(path, data, *executable);
    }
    let checksum = mikros_mika::format::checksum_text(&package.checksum(None));
    let text = manifest(app, &checksum)?;
    let at = dir.join(app.id).join("manifest.yaml");
    if fs::read(&at).ok().as_deref() != Some(text.as_bytes()) {
        fs::create_dir_all(at.parent().context("a manifest in a folder")?)?;
        fs::write(&at, &text).with_context(|| format!("writing {}", at.display()))?;
    }
    let folder = format!("system/apps/{}", app.id);
    let mut members: Vec<(String, PathBuf)> = app
        .files
        .iter()
        .map(|(path, file, _)| (format!("{folder}/{path}"), file.clone()))
        .collect();
    members.push((format!("{folder}/manifest.yaml"), at));
    Ok(members)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(files: Vec<(String, PathBuf, bool)>) -> SystemApp<'static> {
        SystemApp {
            id: "system.mikros.probe",
            version: "1.0.0",
            name: "Probe",
            description: "A probe",
            executable: "probe",
            portable: false,
            installable: false,
            shell_parts: "  - roles: [greeter]\n    executable: probe\n",
            files,
        }
    }

    #[test]
    fn writes_a_manifest_with_the_checksum_of_the_files() {
        let dir = std::env::temp_dir().join(format!("system-apps-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let program = dir.join("probe");
        fs::write(&program, b"program").unwrap();
        let members = members(&app(vec![("probe".into(), program.clone(), true)]), &dir).unwrap();
        let names: Vec<&str> = members.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "system/apps/system.mikros.probe/probe",
                "system/apps/system.mikros.probe/manifest.yaml"
            ]
        );
        let text = fs::read(&members[1].1).unwrap();
        let parsed = mikros_mika::manifest::Manifest::parse(&text).unwrap();
        let mut package = mikros_mika::format::Builder::default();
        package.file("probe", b"program".to_vec(), true);
        let expected = mikros_mika::format::checksum_text(&package.checksum(None));
        assert_eq!(parsed.checksum.as_deref(), Some(expected.as_str()));
        assert_eq!(parsed.executable.as_deref(), Some("probe"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_ids_outside_the_system_apps() {
        let mut other = app(Vec::new());
        other.id = "org.example.probe";
        assert!(members(&other, Path::new("unused")).is_err());
    }
}
