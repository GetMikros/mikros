//! What rmdir takes, and the folders it removes for each path
//! (MIKSPEC-0098.R37).

use mikros_cli::{Kind, Operand, Opt, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::some(
        "folder",
        Kind::Folder,
        "the empty folders to remove",
    )],
    options: &[
        Opt::both(
            "parents",
            'p',
            "remove the folders above each one that its path names, too",
        ),
        Opt::both("verbose", 'v', "name each folder removed"),
    ],
    ..Spec::new("rmdir", "Removes empty folders.")
};

/// The folders to remove for `path`, in order: the path itself, and with
/// `parents` each folder above it that the path names, as `a/b/c`,
/// `a/b` and `a`.
pub fn folders(path: &str, parents: bool) -> Vec<String> {
    let mut path = trimmed(path);
    let mut out = vec![path.to_string()];
    while let Some(at) = path.rfind('/').filter(|_| parents) {
        path = trimmed(&path[..at]);
        if path.is_empty() || path == "/" {
            break;
        }
        out.push(path.to_string());
    }
    out
}

/// `path` without the slashes at its end, unless it is all slashes.
fn trimmed(path: &str) -> &str {
    match path.trim_end_matches('/') {
        "" if path.starts_with('/') => "/",
        t => t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_folders_above() {
        assert_eq!(folders("a/b/c", false), ["a/b/c"]);
        assert_eq!(folders("a/b//c/", true), ["a/b//c", "a/b", "a"]);
        assert_eq!(folders("/x/y", true), ["/x/y", "/x"]);
        assert_eq!(folders("a", true), ["a"]);
        assert_eq!(folders("/", true), ["/"]);
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/rmdir.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
