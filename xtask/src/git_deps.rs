//! The crates of the user workspace that come from git repositories are
//! pinned by commit (MIKSPEC-0062.R21): a dependency with `git` has `rev`,
//! 40 lowercase hexadecimal digits, and neither `branch` nor `tag`, and the
//! source of each such package in `Cargo.lock` has the form
//! `git+<address>?rev=<rev>#<commit>` with `<rev>` equal to `<commit>`.

use anyhow::{Context, Result};
use std::path::Path;

/// Whether `text` is 40 lowercase hexadecimal digits.
fn is_commit(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The problems of one dependency table `table` (`[dependencies]` or
/// `[workspace.dependencies]`), by the name of the package that lists it.
fn check_table(package: &str, table: &toml::Table, problems: &mut Vec<String>) {
    for (name, value) in table {
        let Some(dependency) = value.as_table() else {
            continue;
        };
        if !dependency.contains_key("git") {
            continue;
        }
        let rev = dependency.get("rev").and_then(toml::Value::as_str);
        if !rev.is_some_and(is_commit) {
            problems.push(format!(
                "{package}: the git dependency {name} has no `rev` of 40 lowercase hexadecimal digits"
            ));
        }
        for key in ["branch", "tag"] {
            if dependency.contains_key(key) {
                problems.push(format!(
                    "{package}: the git dependency {name} names a `{key}`, which a git dependency must not"
                ));
            }
        }
    }
}

/// The problems of the manifest `text` of the package `package`: its
/// `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, those of
/// its target tables and, for a workspace manifest, `[workspace.dependencies]`.
pub fn check_manifest(package: &str, text: &str) -> Result<Vec<String>> {
    let manifest: toml::Table = text
        .parse()
        .with_context(|| format!("{package}: reading Cargo.toml"))?;
    let mut problems = Vec::new();
    let mut tables: Vec<&toml::Table> = Vec::new();
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(t) = manifest.get(key).and_then(toml::Value::as_table) {
            tables.push(t);
        }
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target in targets.values().filter_map(toml::Value::as_table) {
            for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(t) = target.get(key).and_then(toml::Value::as_table) {
                    tables.push(t);
                }
            }
        }
    }
    if let Some(t) = manifest
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|w| w.get("dependencies"))
        .and_then(toml::Value::as_table)
    {
        tables.push(t);
    }
    for table in tables {
        check_table(package, table, &mut problems);
    }
    Ok(problems)
}

/// The problems of the lock file `text`: a package from a git repository
/// whose source is not `git+<address>?rev=<rev>#<commit>` with the same
/// 40 lowercase hexadecimal digits as `<rev>` and `<commit>`.
pub fn check_lock(text: &str) -> Result<Vec<String>> {
    let lock: toml::Table = text.parse().context("reading Cargo.lock")?;
    let mut problems = Vec::new();
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    for package in packages.iter().filter_map(toml::Value::as_table) {
        let name = package
            .get("name")
            .and_then(toml::Value::as_str)
            .unwrap_or("?");
        let Some(source) = package.get("source").and_then(toml::Value::as_str) else {
            continue;
        };
        let Some(rest) = source.strip_prefix("git+") else {
            continue;
        };
        let ok = rest.split_once('#').is_some_and(|(address, commit)| {
            address.split_once("?rev=").is_some_and(|(before, rev)| {
                !before.is_empty()
                    && !before.contains(['?', '#'])
                    && !rev.contains(['?', '&', '#'])
                    && is_commit(rev)
                    && rev == commit
            })
        });
        if !ok {
            problems.push(format!(
                "{name}: its source {source} is not git+<address>?rev=<rev>#<commit> with the same commit in both"
            ));
        }
    }
    Ok(problems)
}

/// The problems of the git dependencies of the workspace `user` and its
/// members, whose manifests are `manifests`, by package name.
pub fn check(user: &Path, manifests: &[(String, std::path::PathBuf)]) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    let root = user.join("Cargo.toml");
    let text =
        std::fs::read_to_string(&root).with_context(|| format!("reading {}", root.display()))?;
    problems.extend(check_manifest("the workspace", &text)?);
    for (name, path) in manifests {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        problems.extend(check_manifest(name, &text)?);
    }
    let lock = user.join("Cargo.lock");
    if lock.is_file() {
        let text = std::fs::read_to_string(&lock)
            .with_context(|| format!("reading {}", lock.display()))?;
        problems.extend(check_lock(&text)?);
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REV: &str = "0123456789abcdef0123456789abcdef01234567";

    fn manifest(dependency: &str) -> String {
        format!("[package]\nname = \"x\"\n[dependencies]\ny = {dependency}\n")
    }

    #[test]
    fn accepts_a_git_dependency_with_a_commit() {
        let text = manifest(&format!(
            "{{ git = \"https://example.org/y\", rev = \"{REV}\" }}"
        ));
        assert!(check_manifest("x", &text).unwrap().is_empty());
        assert!(
            check_manifest("x", &manifest("\"1.0\""))
                .unwrap()
                .is_empty()
        );
        assert!(
            check_manifest("x", &manifest("{ path = \"../y\" }"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn names_the_package_for_a_git_dependency_without_a_commit() {
        for bad in [
            "{ git = \"https://example.org/y\" }".to_owned(),
            "{ git = \"https://example.org/y\", branch = \"main\" }".to_owned(),
            "{ git = \"https://example.org/y\", tag = \"v1\" }".to_owned(),
            "{ git = \"https://example.org/y\", rev = \"main\" }".to_owned(),
            format!(
                "{{ git = \"https://example.org/y\", rev = \"{}\" }}",
                REV.to_uppercase()
            ),
            format!(
                "{{ git = \"https://example.org/y\", rev = \"{}\" }}",
                &REV[..39]
            ),
            format!("{{ git = \"https://example.org/y\", rev = \"{REV}\", branch = \"main\" }}"),
        ] {
            let problems = check_manifest("x", &manifest(&bad)).unwrap();
            assert!(!problems.is_empty(), "{bad}");
            assert!(
                problems[0].starts_with("x: the git dependency y"),
                "{problems:?}"
            );
        }
    }

    #[test]
    fn looks_at_the_workspace_and_the_target_tables_too() {
        let workspace = "[workspace.dependencies]\nz = { git = \"https://example.org/z\" }\n";
        assert_eq!(check_manifest("w", workspace).unwrap().len(), 1);
        let target = "[target.'cfg(target_os = \"mikros\")'.dependencies]\n\
                      z = { git = \"https://example.org/z\", tag = \"t\" }\n";
        assert!(!check_manifest("t", target).unwrap().is_empty());
    }

    fn lock(source: &str) -> String {
        format!("[[package]]\nname = \"y\"\nversion = \"1.0.0\"\nsource = \"{source}\"\n")
    }

    #[test]
    fn accepts_a_lock_source_of_the_form_with_the_same_commit() {
        let good = lock(&format!("git+https://example.org/y?rev={REV}#{REV}"));
        assert!(check_lock(&good).unwrap().is_empty());
        let registry = lock("registry+https://github.com/rust-lang/crates.io-index");
        assert!(check_lock(&registry).unwrap().is_empty());
        assert!(
            check_lock("[[package]]\nname = \"local\"\nversion = \"0.0.0\"\n")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn names_a_lock_package_whose_source_has_another_form() {
        let other = "fedcba9876543210fedcba9876543210fedcba98";
        for bad in [
            format!("git+https://example.org/y#{REV}"),
            format!("git+https://example.org/y?branch=main#{REV}"),
            format!("git+https://example.org/y?tag=v1#{REV}"),
            format!("git+https://example.org/y?rev={REV}#{other}"),
            format!("git+https://example.org/y?rev=main#{REV}"),
            format!("git+https://example.org/y?rev={REV}"),
            format!("git+?rev={REV}#{REV}"),
        ] {
            let problems = check_lock(&lock(&bad)).unwrap();
            assert_eq!(problems.len(), 1, "{bad}");
            assert!(problems[0].starts_with("y: its source"), "{problems:?}");
        }
    }

    #[test]
    fn the_user_workspace_passes() {
        let user = crate::workspace_root().join("user");
        let manifests = [];
        assert!(check(&user, &manifests).unwrap().is_empty());
    }
}
