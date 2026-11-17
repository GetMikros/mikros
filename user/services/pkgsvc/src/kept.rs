//! Which old versions of library packages are kept (MIKSPEC-0101.R51): an
//! update keeps the old version when it removes a library that a program
//! of an installed package names in `DT_NEEDED`, and a kept version goes
//! once no such program names a library that only it holds.

use std::collections::BTreeSet;

/// The suffix of a library's file.
const SUFFIX: &str = ".dlib";

/// The paths, in the library package `id`, of the libraries that the
/// names `needed` of `DT_NEEDED` entries stand for: `<path>.dlib` for each
/// `<id>:<path>`.
pub fn named(needed: &BTreeSet<String>, id: &str) -> BTreeSet<String> {
    needed
        .iter()
        .filter_map(|name| name.split_once(':'))
        .filter(|(of, path)| *of == id && !path.is_empty())
        .map(|(_, path)| format!("{path}{SUFFIX}"))
        .collect()
}

/// Whether updating the library package `id` from the files `old` to the
/// files `new` removes a library that `needed`, the names that programs of
/// installed packages need, names, so that the old version is kept.
pub fn keeps(
    id: &str,
    old: &BTreeSet<String>,
    new: &BTreeSet<String>,
    needed: &BTreeSet<String>,
) -> bool {
    named(needed, id)
        .iter()
        .any(|file| old.contains(file) && !new.contains(file))
}

/// Whether the kept version of `id` with the files `kept` is still needed:
/// a program names a library that it holds and the current version, with
/// the files `current`, does not.
pub fn still_needed(
    id: &str,
    kept: &BTreeSet<String>,
    current: &BTreeSet<String>,
    needed: &BTreeSet<String>,
) -> bool {
    named(needed, id)
        .iter()
        .any(|file| kept.contains(file) && !current.contains(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn keeps_an_old_version_whose_library_a_program_still_names() {
        let old = set(&["3/glfw.dlib", "manifest.yaml"]);
        let new = set(&["4/glfw.dlib", "manifest.yaml"]);
        let needed = set(&["org.glfw.dev:3/glfw", "libc.so", "org.other:3/glfw"]);
        assert!(keeps("org.glfw.dev", &old, &new, &needed));
        // Nothing names the removed library, or the new version still has it.
        assert!(!keeps(
            "org.glfw.dev",
            &old,
            &new,
            &set(&["org.glfw.dev:4/glfw"])
        ));
        assert!(!keeps("org.glfw.dev", &old, &old, &needed));
    }

    #[test]
    fn lets_a_kept_version_go_once_nothing_needs_it() {
        let kept = set(&["3/glfw.dlib"]);
        let current = set(&["4/glfw.dlib"]);
        assert!(still_needed(
            "org.glfw.dev",
            &kept,
            &current,
            &set(&["org.glfw.dev:3/glfw"])
        ));
        assert!(!still_needed(
            "org.glfw.dev",
            &kept,
            &current,
            &set(&["org.glfw.dev:4/glfw"])
        ));
        // A library that the current version holds too is found there.
        assert!(!still_needed(
            "org.glfw.dev",
            &kept,
            &kept,
            &set(&["org.glfw.dev:3/glfw"])
        ));
    }
}
