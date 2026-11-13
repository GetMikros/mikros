use super::*;
use std::collections::BTreeMap;

/// A tree of folders: each path with its entries.
struct Tree(BTreeMap<&'static str, Vec<(&'static str, bool)>>);

impl Folders for Tree {
    fn entries(&self, path: &str) -> Option<Vec<(String, bool)>> {
        self.0
            .get(path)
            .map(|e| e.iter().map(|(n, d)| (n.to_string(), *d)).collect())
    }
}

fn tree() -> Tree {
    Tree(BTreeMap::from([
        (
            ".",
            vec![
                ("a.txt", false),
                ("b.txt", false),
                (".hidden.txt", false),
                ("src", true),
                ("c.rs", false),
            ],
        ),
        ("src", vec![("main.rs", false), ("lib", true)]),
        ("src/lib", vec![("x.rs", false), ("y.c", false)]),
        ("/etc", vec![("hosts", false)]),
        ("/", vec![("etc", true)]),
    ]))
}

fn glob(pattern: &str) -> Result<Vec<String>, String> {
    expand(&[Piece::Pattern(pattern.into())], &tree())
}

#[test]
fn matches_stars_questions_and_classes() {
    assert_eq!(glob("*.txt").unwrap(), ["a.txt", "b.txt"]);
    assert_eq!(glob("?.txt").unwrap(), ["a.txt", "b.txt"]);
    assert_eq!(glob("[a].txt").unwrap(), ["a.txt"]);
    assert_eq!(glob("[!a].txt").unwrap(), ["b.txt"]);
    assert_eq!(glob("[a-b].*").unwrap(), ["a.txt", "b.txt"]);
    assert_eq!(glob(".*.txt").unwrap(), [".hidden.txt"]);
    assert_eq!(glob("src/*.rs").unwrap(), ["src/main.rs"]);
    assert_eq!(glob("/etc/h*").unwrap(), ["/etc/hosts"]);
}

#[test]
fn walks_folders_with_double_stars() {
    assert_eq!(
        glob("**/*.rs").unwrap(),
        ["c.rs", "src/lib/x.rs", "src/main.rs"]
    );
    assert_eq!(glob("src/**/*.c").unwrap(), ["src/lib/y.c"]);
}

#[test]
fn fails_to_match_nothing_and_keeps_literals_literal() {
    assert_eq!(glob("*.md"), Err("no file matches `*.md`".into()));
    // A literal `*` from quotes or a variable matches only itself.
    let pieces = [Piece::Literal("*".into()), Piece::Pattern(".txt".into())];
    assert!(expand(&pieces, &tree()).is_err());
    assert!(has_wildcards("a*"));
    assert!(has_wildcards("f[12]"));
    assert!(!has_wildcards("plain.txt"));
}
