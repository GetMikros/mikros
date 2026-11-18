//! The access policy of the protocols in `midl/` (MIKSPEC-0023.R47):
//! beside each MIDL file that defines a protocol, a file of the same name
//! with the extension `policy` that holds, for every method of each of
//! its protocols, a line `<Protocol>.<Method>: <access>`, where `<access>`
//! is `permission:<permission>`, the permission that the method requires;
//! `data-dependent:<function>`, the server function that decides from the
//! request; or `public`, for a method that every client of the channel
//! may call. Empty lines and lines that start with `#` are comments.
//! Events, which only servers send, have no entry.

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::Path;

/// The problems of the policy files of every MIDL file under `midl/`.
pub fn problems() -> Result<Vec<String>> {
    let root = crate::workspace_root();
    let mut files = Vec::new();
    for library in std::fs::read_dir(root.join("midl")).context("reading midl/")? {
        let library = library?.path();
        if !library.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&library)? {
            let file = file?.path();
            if file.extension().is_some_and(|e| e == "midl") {
                files.push(file);
            }
        }
    }
    files.sort();
    let mut problems = Vec::new();
    for file in files {
        let name = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .display()
            .to_string();
        let text = std::fs::read_to_string(&file).with_context(|| format!("reading {name}"))?;
        let policy = std::fs::read_to_string(file.with_extension("policy")).ok();
        problems.extend(check(&name, &text, policy.as_deref()));
    }
    Ok(problems)
}

/// The methods, as `<Protocol>.<Method>`, of the protocols that the MIDL
/// source `text` of the file `name` defines, without their events.
fn methods(name: &str, text: &str) -> Result<Vec<String>, String> {
    let file = midlc::parse(name, text).map_err(|e| format!("{name}: {e}"))?;
    let mut methods = Vec::new();
    for decl in &file.decls {
        if let midlc::ast::DeclKind::Protocol(protocol) = &decl.kind {
            methods.extend(
                protocol
                    .iter()
                    .filter(|m| !matches!(m.kind, midlc::ast::MethodKind::Event(_)))
                    .map(|m| format!("{}.{}", decl.name, m.name)),
            );
        }
    }
    Ok(methods)
}

/// Whether `access` is one of the three forms of an entry.
fn valid_access(access: &str) -> bool {
    if access == "public" {
        return true;
    }
    if let Some(permission) = access.strip_prefix("permission:") {
        let parts: Vec<&str> = permission.split('.').collect();
        return parts.len() == 3
            && parts.iter().all(|p| {
                p.starts_with(|c: char| c.is_ascii_lowercase())
                    && p.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            });
    }
    if let Some(function) = access.strip_prefix("data-dependent:") {
        return !function.is_empty()
            && function
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':');
    }
    false
}

/// The problems of the policy `policy`, if there is one, of the MIDL file
/// `name` whose source is `text`.
fn check(name: &str, text: &str, policy: Option<&str>) -> Vec<String> {
    let methods = match methods(name, text) {
        Ok(methods) => methods,
        Err(problem) => return vec![problem],
    };
    if methods.is_empty() {
        return Vec::new();
    }
    let policy_name = Path::new(name)
        .with_extension("policy")
        .display()
        .to_string();
    let Some(policy) = policy else {
        return vec![format!("{name} has no access policy {policy_name}")];
    };
    let mut problems = Vec::new();
    let mut named = BTreeSet::new();
    for (i, line) in policy.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = format!("{policy_name}:{}", i + 1);
        let Some((method, access)) = line.split_once(':') else {
            problems.push(format!("{at}: not `<Protocol>.<Method>: <access>`"));
            continue;
        };
        let (method, access) = (method.trim(), access.trim());
        if !methods.iter().any(|m| m == method) {
            problems.push(format!("{at}: {name} defines no method {method}"));
        }
        if !valid_access(access) {
            problems.push(format!(
                "{at}: {method}: `{access}` is not permission:<permission>, \
                 data-dependent:<function> or public"
            ));
        }
        if !named.insert(method.to_owned()) {
            problems.push(format!("{at}: {method} has a second entry"));
        }
    }
    for method in methods.iter().filter(|m| !named.contains(*m)) {
        problems.push(format!("{policy_name}: no entry for {method}"));
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIDL: &str = "library test.x;\nprotocol Thing {\n    @1 Get() -> (n u32);\n    \
        @2 Set(n u32);\n    @3 -> OnChanged(n u32);\n};\n";

    #[test]
    fn lists_methods_but_not_events() {
        assert_eq!(
            methods("x.midl", MIDL),
            Ok(vec!["Thing.Get".to_owned(), "Thing.Set".to_owned()])
        );
    }

    #[test]
    fn accepts_a_policy_with_every_method() {
        let policy = "# Comment\n\nThing.Get: public\nThing.Set: permission:system.things.set\n";
        assert!(check("x.midl", MIDL, Some(policy)).is_empty());
        let policy = "Thing.Get: data-dependent:thingsvc::serve::get\nThing.Set: public\n";
        assert!(check("x.midl", MIDL, Some(policy)).is_empty());
    }

    #[test]
    fn refuses_missing_unknown_and_malformed_entries() {
        assert_eq!(check("x.midl", MIDL, None).len(), 1);
        let missing = check("x.midl", MIDL, Some("Thing.Get: public\n"));
        assert_eq!(missing, vec!["x.policy: no entry for Thing.Set".to_owned()]);
        let unknown = "Thing.Get: public\nThing.Set: public\nThing.Gone: public\n";
        assert_eq!(check("x.midl", MIDL, Some(unknown)).len(), 1);
        let event = "Thing.Get: public\nThing.Set: public\nThing.OnChanged: public\n";
        assert_eq!(check("x.midl", MIDL, Some(event)).len(), 1);
        for access in [
            "anyone",
            "permission:things",
            "data-dependent:",
            "permission:A.b.c",
        ] {
            let policy = format!("Thing.Get: {access}\nThing.Set: public\n");
            assert_eq!(check("x.midl", MIDL, Some(&policy)).len(), 1, "{access}");
        }
        let twice = "Thing.Get: public\nThing.Get: public\nThing.Set: public\n";
        assert_eq!(check("x.midl", MIDL, Some(twice)).len(), 1);
    }

    #[test]
    fn needs_no_policy_without_protocols() {
        assert!(
            check(
                "x.midl",
                "library test.x;\ntype N = struct { n u32; };\n",
                None
            )
            .is_empty()
        );
    }
}
