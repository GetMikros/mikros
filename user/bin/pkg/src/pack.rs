//! `pkg pack`: a package from a folder, whose `manifest.yaml` needs only
//! `id`, `kind` and `version`, and gets its `checksum` filled in
//! (MIKSPEC-0101.R28), a pkg-config file for its libraries
//! (MIKSPEC-0101.R52), and the library packages that its programs and
//! libraries need added to its `needs` (MIKSPEC-0101.R29).

use crate::elf;
use mikros_mika::format::{Builder, MANIFEST, checksum_text};
use mikros_mika::manifest::Manifest;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The manifest `text` with its top-level `checksum` set to `checksum`.
pub fn with_checksum(text: &str, checksum: &str) -> String {
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !l.starts_with("checksum:"))
        .map(String::from)
        .collect();
    lines.push(format!("checksum: {checksum}"));
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// A file, folder or link of the folder being packed, by its path in the
/// package.
enum Item {
    Folder,
    File { contents: Vec<u8>, executable: bool },
    Symlink(String),
}

/// Adds the entries below `dir`, at `prefix` in the package, to `items`,
/// leaving out the manifest at the root.
fn collect(items: &mut Vec<(String, Item)>, dir: &Path, prefix: &str) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| format!("{}: a name that is not UTF-8", dir.display()))?;
        let path = match prefix.is_empty() {
            true => name.to_string(),
            false => format!("{prefix}/{name}"),
        };
        if path == MANIFEST {
            continue;
        }
        let meta = fs::symlink_metadata(entry.path()).map_err(|e| format!("{path}: {e}"))?;
        if meta.file_type().is_symlink() {
            let target = fs::read_link(entry.path()).map_err(|e| format!("{path}: {e}"))?;
            let target = target
                .to_str()
                .ok_or_else(|| format!("{path}: a target that is not UTF-8"))?;
            items.push((path, Item::Symlink(target.into())));
        } else if meta.is_dir() {
            items.push((path.clone(), Item::Folder));
            collect(items, &entry.path(), &path)?;
        } else {
            let contents = fs::read(entry.path()).map_err(|e| format!("{path}: {e}"))?;
            let executable = meta.permissions().mode() & 0o111 != 0;
            items.push((
                path,
                Item::File {
                    contents,
                    executable,
                },
            ));
        }
    }
    Ok(())
}

/// The library package ID and path of the library name `name` of the form
/// `<id>:<path>` (MIKSPEC-0101.R51).
fn package_name(name: &str) -> Option<(&str, &str)> {
    name.split_once(':')
        .filter(|(id, path)| mikros_mika::id::is_valid(id) && !path.is_empty())
}

/// The pkg-config file of the libraries `paths` of the package `id`, in
/// `version` (MIKSPEC-0101.R52).
pub fn pkg_config(id: &str, description: &str, version: &str, paths: &[&str]) -> String {
    let libs: Vec<String> = paths
        .iter()
        .map(|p| format!("${{prefix}}/{p}.dlib"))
        .collect();
    format!(
        "prefix=${{pcfiledir}}/../..\n\nName: {id}\nDescription: {description}\n\
         Version: {version}\nLibs: {}\nCflags: -I${{prefix}}/include\n",
        libs.join(" ")
    )
}

/// The manifest `text` with `added`, pairs of an ID and a version range,
/// added to its top-level map `needs`.
pub fn with_needs(text: &str, added: &[(String, String)]) -> String {
    if added.is_empty() {
        return text.into();
    }
    let lines: Vec<&str> = text.lines().collect();
    let entries: Vec<String> = added
        .iter()
        .map(|(id, range)| format!("  {id}: '{range}'"))
        .collect();
    let mut out: Vec<String> = Vec::new();
    match lines.iter().position(|l| l.trim_end() == "needs:") {
        Some(at) => {
            // After the last line of the map, which is indented.
            let mut end = at + 1;
            while end < lines.len() && lines[end].starts_with(' ') {
                end += 1;
            }
            out.extend(lines[..end].iter().map(|l| l.to_string()));
            out.extend(entries);
            out.extend(lines[end..].iter().map(|l| l.to_string()));
        }
        None => {
            out.extend(lines.iter().map(|l| l.to_string()));
            out.push("needs:".into());
            out.extend(entries);
        }
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// The package of `folder`, the ID and version it is of, and warnings
/// about libraries that it needs and that `versions`, the version of an
/// installed or a sysroot's library package by its ID, does not know.
pub fn pack(
    folder: &Path,
    versions: &dyn Fn(&str) -> Option<String>,
) -> Result<(Vec<u8>, Manifest, Vec<String>), String> {
    let text = fs::read_to_string(folder.join(MANIFEST))
        .map_err(|e| format!("{}: {e}", folder.join(MANIFEST).display()))?;
    let manifest =
        Manifest::parse(text.as_bytes()).map_err(|e| format!("{MANIFEST}: {} {}", e.key, e.why))?;
    if manifest.kind.is_none() {
        return Err(format!("{MANIFEST}: kind is missing"));
    }
    let Some(version) = manifest.version.as_ref().map(|v| v.to_string()) else {
        return Err(format!("{MANIFEST}: version is missing"));
    };
    let mut items = Vec::new();
    collect(&mut items, folder, "")?;
    // The libraries of the package by their package IDs, and the library
    // names that its images need.
    let mut libraries: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut needed = BTreeSet::new();
    for (_, item) in &items {
        let Item::File { contents, .. } = item else {
            continue;
        };
        let Some(names) = elf::names(contents) else {
            continue;
        };
        if let Some((id, path)) = names.soname.as_deref().and_then(package_name) {
            libraries.entry(id.into()).or_default().push(path.into());
        }
        needed.extend(names.needed);
    }
    // A pkg-config file for each library package ID (MIKSPEC-0101.R52).
    let description = manifest
        .description
        .as_deref()
        .unwrap_or(&manifest.id)
        .to_string();
    let mut pc = Vec::new();
    for (id, paths) in &libraries {
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let file = format!("lib/pkgconfig/{id}.pc");
        pc.push((file, pkg_config(id, &description, &version, &paths)));
    }
    // The library packages that it needs and does not hold
    // (MIKSPEC-0101.R29).
    let mut added = Vec::new();
    let mut warnings = Vec::new();
    let known: BTreeSet<&str> = manifest.needs.iter().map(|(id, _)| id.as_str()).collect();
    let ids: BTreeSet<&str> = needed
        .iter()
        .filter_map(|n| package_name(n))
        .map(|(id, _)| id)
        .collect();
    for id in ids {
        if libraries.contains_key(id) || known.contains(id) || id == manifest.id {
            continue;
        }
        match versions(id) {
            Some(found) => added.push((id.to_string(), format!(">={found}"))),
            None => warnings.push(format!(
                "warning: the library package {id} is needed but cannot be found"
            )),
        }
    }
    let folders: BTreeSet<String> = items
        .iter()
        .filter(|(_, item)| matches!(item, Item::Folder))
        .map(|(path, _)| path.clone())
        .collect();
    let mut builder = Builder::default();
    for (path, item) in items {
        match item {
            Item::Folder => builder.folder(&path),
            Item::File {
                contents,
                executable,
            } => builder.file(&path, contents, executable),
            Item::Symlink(target) => builder.symlink(&path, &target),
        }
    }
    if !pc.is_empty() {
        for folder in ["lib", "lib/pkgconfig"] {
            if !folders.contains(folder) {
                builder.folder(folder);
            }
        }
        for (path, contents) in pc {
            builder.file(&path, contents.into_bytes(), false);
        }
    }
    let text = with_needs(&text, &added);
    let checksum = checksum_text(&builder.checksum(manifest.signature.as_deref()));
    let text = with_checksum(&text, &checksum);
    let manifest =
        Manifest::parse(text.as_bytes()).map_err(|e| format!("{MANIFEST}: {} {}", e.key, e.why))?;
    builder.file(MANIFEST, text.into_bytes(), false);
    Ok((builder.finish(), manifest, warnings))
}

/// The version of the library package `id` installed at `libraries`, the
/// folder of library packages, from its manifest.
pub fn version_in(libraries: &Path, id: &str) -> Option<String> {
    let bytes = fs::read(libraries.join(id).join(MANIFEST)).ok()?;
    Some(Manifest::parse(&bytes).ok()?.version?.to_string())
}

/// Packs `folder` into `out`, or `<id>-<version>.mika`, taking the
/// versions of needed library packages from the folder `libraries`, and
/// prints the file's name and the warnings; the exit code.
pub fn run(folder: &str, out: Option<&str>, libraries: &Path) -> u8 {
    let versions = |id: &str| version_in(libraries, id);
    let (bytes, manifest, warnings) = match pack(Path::new(folder), &versions) {
        Ok(packed) => packed,
        Err(why) => {
            eprintln!("pkg: {why}");
            return mikros_cli::FAILURE;
        }
    };
    for warning in warnings {
        eprintln!("pkg: {warning}");
    }
    let version = manifest.version.map(|v| v.to_string()).unwrap_or_default();
    let out = out
        .map(String::from)
        .unwrap_or(format!("{}-{version}.mika", manifest.id));
    if let Err(e) = fs::write(&out, bytes) {
        eprintln!("pkg: {out}: {e}");
        return mikros_cli::FAILURE;
    }
    println!("{out}");
    mikros_cli::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_mika::format::Package;

    #[test]
    fn fills_in_the_checksum() {
        assert_eq!(
            with_checksum("id: a.b\nchecksum: old\nkind: app\n", "sha256:00"),
            "id: a.b\nkind: app\nchecksum: sha256:00\n"
        );
    }

    #[test]
    fn packs_a_folder() {
        let dir = std::env::temp_dir().join(format!("pkg-pack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(
            dir.join(MANIFEST),
            "id: org.example.tool\nkind: app\nversion: 0.1.0\n",
        )
        .unwrap();
        fs::write(dir.join("bin/tool"), b"program").unwrap();
        fs::set_permissions(dir.join("bin/tool"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.join("README"), b"read me").unwrap();
        std::os::unix::fs::symlink("bin/tool", dir.join("run")).unwrap();
        let (bytes, manifest, warnings) = pack(&dir, &|_| None).unwrap();
        assert!(warnings.is_empty());
        let package = Package::open(&bytes[..]).unwrap();
        assert!(package.find("bin/tool").unwrap().executable());
        assert!(!package.find("README").unwrap().executable());
        assert_eq!(
            package
                .read(&bytes[..], package.find("run").unwrap())
                .unwrap(),
            b"bin/tool"
        );
        // The checksum that pack filled in is the package's.
        let checksum = checksum_text(&package.checksum(None));
        assert_eq!(manifest.checksum.as_deref(), Some(checksum.as_str()));
        fs::write(dir.join(MANIFEST), "id: org.example.tool\nkind: app\n").unwrap();
        assert_eq!(
            pack(&dir, &|_| None).unwrap_err(),
            "manifest.yaml: version is missing"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writes_pkg_config_files() {
        assert_eq!(
            pkg_config("org.glfw.dev", "Windows.", "3.4.0", &["3/glfw"]),
            "prefix=${pcfiledir}/../..\n\nName: org.glfw.dev\nDescription: Windows.\n\
             Version: 3.4.0\nLibs: ${prefix}/3/glfw.dlib\nCflags: -I${prefix}/include\n"
        );
    }

    #[test]
    fn adds_needs_to_the_manifest() {
        let added = [("org.b.lib".to_string(), ">=1.2.0".to_string())];
        assert_eq!(
            with_needs(
                "id: a.b\nneeds:\n  org.a.lib: '>=1.0.0'\nkind: app\n",
                &added
            ),
            "id: a.b\nneeds:\n  org.a.lib: '>=1.0.0'\n  org.b.lib: '>=1.2.0'\nkind: app\n"
        );
        assert_eq!(
            with_needs("id: a.b\nkind: app\n", &added),
            "id: a.b\nkind: app\nneeds:\n  org.b.lib: '>=1.2.0'\n"
        );
    }

    #[test]
    fn packs_libraries_and_what_they_need() {
        use crate::elf::tests::image;
        let dir = std::env::temp_dir().join(format!("pkg-pack-lib-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("3")).unwrap();
        fs::write(
            dir.join(MANIFEST),
            "id: org.example.lib\nkind: library\nversion: 3.1.0\ndescription: Draws.\n",
        )
        .unwrap();
        let needs = [
            "org.other.lib:other",
            "org.gone.lib:gone",
            "org.example.lib:3/a",
            "libc.so",
        ];
        fs::write(
            dir.join("3/a.dlib"),
            image(&needs, Some("org.example.lib:3/a")),
        )
        .unwrap();
        let versions = |id: &str| (id == "org.other.lib").then(|| "2.0.1".to_string());
        let (bytes, manifest, warnings) = pack(&dir, &versions).unwrap();
        let package = Package::open(&bytes[..]).unwrap();
        let pc = package.find("lib/pkgconfig/org.example.lib.pc").unwrap();
        let pc = String::from_utf8(package.read(&bytes[..], pc).unwrap()).unwrap();
        assert!(pc.contains("Libs: ${prefix}/3/a.dlib\n") && pc.contains("Version: 3.1.0\n"));
        // Only the library package that is found, not its own nor the
        // system's libraries.
        assert_eq!(manifest.needs.len(), 1);
        assert_eq!(manifest.needs[0].0, "org.other.lib");
        assert_eq!(
            warnings,
            ["warning: the library package org.gone.lib is needed but cannot be found"]
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
