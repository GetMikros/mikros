//! The paths of namespace entries, as the root namespace resolves them.

use alloc::string::String;
use alloc::vec::Vec;

/// The absolute `path` with empty and `.` components removed and `..`
/// components resolved lexically, never above `/`; `None` for a path
/// that is not absolute.
pub fn normalize(path: &str) -> Option<String> {
    let rest = path.strip_prefix('/')?;
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let mut out = String::new();
    for part in &parts {
        out.push('/');
        out.push_str(part);
    }
    if out.is_empty() {
        out.push('/');
    }
    Some(out)
}

/// Whether the normalized paths `a` and `b` are the same, or one lies
/// below the other.
pub fn overlaps(a: &str, b: &str) -> bool {
    within(a, b) || within(b, a)
}

/// Whether the normalized `path` is `dir` or lies below it.
fn within(path: &str, dir: &str) -> bool {
    match path.strip_prefix(dir) {
        Some(rest) => rest.is_empty() || dir == "/" || rest.starts_with('/'),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_normalized_lexically() {
        let n = |p| normalize(p);
        assert_eq!(n("/"), Some("/".into()));
        assert_eq!(n("/var/lib/"), Some("/var/lib".into()));
        assert_eq!(n("//var//./lib"), Some("/var/lib".into()));
        assert_eq!(
            n("/etc/../var/lib/accounts"),
            Some("/var/lib/accounts".into())
        );
        assert_eq!(n("/../../etc"), Some("/etc".into()));
        assert_eq!(n("/etc/.."), Some("/".into()));
        assert_eq!(n("/a/..b/.c"), Some("/a/..b/.c".into()));
        assert_eq!(n("etc"), None);
        assert_eq!(n(""), None);
    }

    #[test]
    fn paths_overlap_at_component_boundaries() {
        let data = "/var/lib/accounts";
        for yes in [
            "/var/lib/accounts",
            "/var/lib/accounts/users",
            "/var/lib",
            "/var",
            "/",
        ] {
            assert!(overlaps(yes, data), "{yes}");
            assert!(overlaps(data, yes), "{yes}");
        }
        for no in [
            "/var/lib/accounts2",
            "/var/lib/account",
            "/var/library",
            "/etc",
            "/usr/var",
        ] {
            assert!(!overlaps(no, data), "{no}");
            assert!(!overlaps(data, no), "{no}");
        }
    }
}
