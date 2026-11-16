//! `pkg pack`: a package from a folder, whose `manifest.yaml` needs only
//! `id`, `kind` and `version`, and gets its `checksum` filled in
//! (MIKSPEC-0101.R28).

use mikros_mika::format::{Builder, MANIFEST, checksum_text};
use mikros_mika::manifest::Manifest;
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

/// Adds the entries below `dir`, at `prefix` in the package, to
/// `builder`, leaving out the manifest at the root.
fn add(builder: &mut Builder, dir: &Path, prefix: &str) -> Result<(), String> {
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
            builder.symlink(&path, target);
        } else if meta.is_dir() {
            builder.folder(&path);
            add(builder, &entry.path(), &path)?;
        } else {
            let contents = fs::read(entry.path()).map_err(|e| format!("{path}: {e}"))?;
            builder.file(&path, contents, meta.permissions().mode() & 0o111 != 0);
        }
    }
    Ok(())
}

/// The package of `folder`, and the ID and version it is of.
pub fn pack(folder: &Path) -> Result<(Vec<u8>, Manifest), String> {
    let text = fs::read_to_string(folder.join(MANIFEST))
        .map_err(|e| format!("{}: {e}", folder.join(MANIFEST).display()))?;
    let manifest =
        Manifest::parse(text.as_bytes()).map_err(|e| format!("{MANIFEST}: {} {}", e.key, e.why))?;
    if manifest.kind.is_none() {
        return Err(format!("{MANIFEST}: kind is missing"));
    }
    if manifest.version.is_none() {
        return Err(format!("{MANIFEST}: version is missing"));
    }
    let mut builder = Builder::default();
    add(&mut builder, folder, "")?;
    let checksum = checksum_text(&builder.checksum(manifest.signature.as_deref()));
    let text = with_checksum(&text, &checksum);
    let manifest =
        Manifest::parse(text.as_bytes()).map_err(|e| format!("{MANIFEST}: {} {}", e.key, e.why))?;
    builder.file(MANIFEST, text.into_bytes(), false);
    Ok((builder.finish(), manifest))
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
        let (bytes, manifest) = pack(&dir).unwrap();
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
        assert_eq!(pack(&dir).unwrap_err(), "manifest.yaml: version is missing");
        fs::remove_dir_all(&dir).unwrap();
    }
}
