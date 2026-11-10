extern crate std;

use crate::*;
use alloc::string::ToString;
use alloc::vec;

fn root(text: &str) -> Node {
    match parse(text.as_bytes()) {
        Ok(node) => node,
        Err(e) => panic!("{e:?} in {text:?}"),
    }
}

fn kind(text: &str) -> ErrorKind {
    match parse(text.as_bytes()) {
        Ok(node) => panic!("accepted {text:?} as {node:?}"),
        Err(e) => e.kind,
    }
}

fn get<'a>(node: &'a Node, key: &str) -> &'a Node {
    node.map()
        .unwrap()
        .0
        .iter()
        .find(|e| e.key == key)
        .map(|e| &e.value)
        .unwrap_or_else(|| panic!("no {key}"))
}

fn text<'a>(node: &'a Node, key: &str) -> &'a str {
    get(node, key).string().unwrap()
}

fn strings(node: &Node) -> std::vec::Vec<&str> {
    node.list()
        .unwrap()
        .iter()
        .map(|n| n.string().unwrap())
        .collect()
}

#[test]
fn reads_block_maps_and_lists() {
    let doc = root(
        "---\n# a comment\nname: boot-fs  # trailing\nafter:\n  - one\n  - 'two'\nlist:\n- a\n- b\nnested:\n  inner: \"x\"\n  deeper:\n    k: v\nempty:\nlast: ~\n",
    );
    assert_eq!(text(&doc, "name"), "boot-fs");
    assert_eq!(strings(get(&doc, "after")), ["one", "two"]);
    assert_eq!(strings(get(&doc, "list")), ["a", "b"]);
    let nested = get(&doc, "nested");
    assert_eq!(text(nested, "inner"), "x");
    assert_eq!(text(get(nested, "deeper"), "k"), "v");
    let map = doc.map().unwrap();
    assert!(map.get("empty").is_none());
    assert!(map.get("last").is_none());
    assert!(map.get("name").is_some());
}

#[test]
fn reads_compact_maps_in_lists() {
    let doc = root(
        "provides:\n  - protocol: mikros.fs.Node\n    permission: files.read\n  - protocol: x\n",
    );
    let items = get(&doc, "provides").list().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(text(&items[0], "protocol"), "mikros.fs.Node");
    assert_eq!(text(&items[0], "permission"), "files.read");
    assert_eq!(text(&items[1], "protocol"), "x");
    let nested = root("a:\n- - x\n  - y\n- z\n");
    let items = get(&nested, "a").list().unwrap();
    assert_eq!(strings(&items[0]), ["x", "y"]);
    assert_eq!(items[1].string().unwrap(), "z");
}

#[test]
fn keeps_scalars_as_text() {
    let doc = root("a: no\nb: 0755\nc: 1.10\nd: true\ne: 'true'\nf: -12\ng: 0x1F\nh: 12a\n");
    assert_eq!(text(&doc, "a"), "no");
    assert_eq!(text(&doc, "b"), "0755");
    assert_eq!(text(&doc, "c"), "1.10");
    assert_eq!(get(&doc, "d").boolean(), Ok(true));
    assert_eq!(
        get(&doc, "e").boolean().unwrap_err().kind,
        ErrorKind::WrongType
    );
    assert_eq!(get(&doc, "f").integer(-100, 100), Ok(-12));
    assert_eq!(get(&doc, "g").integer(0, 100), Ok(31));
    assert_eq!(
        get(&doc, "g").integer(0, 30).unwrap_err().kind,
        ErrorKind::OutOfRange
    );
    assert_eq!(
        get(&doc, "h").integer(0, 100).unwrap_err().kind,
        ErrorKind::WrongType
    );
    assert_eq!(get(&doc, "b").integer(0, 1000), Ok(755));
    assert_eq!(
        get(&doc, "a").list().unwrap_err().kind,
        ErrorKind::WrongType
    );
}

#[test]
fn reads_quoted_scalars() {
    let doc = root(
        "a: 'it''s # not a comment'\nb: \"tab\\there \\u00e9\\x41 \\\"q\\\"\"\nc: \"one\n  two\n\n  three\"\nd: \"joined\\\n   here\"\n'quoted key': 1\n",
    );
    assert_eq!(text(&doc, "a"), "it's # not a comment");
    assert_eq!(text(&doc, "b"), "tab\there \u{e9}A \"q\"");
    assert_eq!(text(&doc, "c"), "one two\nthree");
    assert_eq!(text(&doc, "d"), "joinedhere");
    assert_eq!(text(&doc, "quoted key"), "1");
    assert_eq!(kind("a: \"\\q\"\n"), ErrorKind::BadEscape);
    assert_eq!(kind("a: 'open\n"), ErrorKind::Unterminated);
}

#[test]
fn folds_plain_scalars_over_lines() {
    let doc = root("a: one\n  two\n\n  three # c\nb: x\n");
    assert_eq!(text(&doc, "a"), "one two\nthree");
    assert_eq!(text(&doc, "b"), "x");
    assert_eq!(text(&root("u: a:b c#d\n"), "u"), "a:b c#d");
}

#[test]
fn reads_flow_collections() {
    let doc = root(
        "a: [x, 'y', {k: v, m: [1, 2]}]\nb: {p: q, empty:, last}\nc: []\nd: [\n  one,\n  two, # c\n]\ne: [k: v]\n",
    );
    let a = get(&doc, "a").list().unwrap();
    assert_eq!(a[0].string().unwrap(), "x");
    assert_eq!(a[1].string().unwrap(), "y");
    assert_eq!(text(&a[2], "k"), "v");
    assert_eq!(strings(get(&a[2], "m")), ["1", "2"]);
    let b = get(&doc, "b");
    assert_eq!(text(b, "p"), "q");
    assert!(get(b, "empty").is_absent());
    assert!(get(b, "last").is_absent());
    assert!(get(&doc, "c").list().unwrap().is_empty());
    assert_eq!(strings(get(&doc, "d")), ["one", "two"]);
    assert_eq!(text(&get(&doc, "e").list().unwrap()[0], "k"), "v");
    assert_eq!(kind("a: [x, y\n"), ErrorKind::Unterminated);
    assert_eq!(kind("a: {[x]: y}\n"), ErrorKind::NonScalarKey);
    assert_eq!(kind("a: {x: 1, x: 2}\n"), ErrorKind::DuplicateKey);
    assert_eq!(text(&root("{a: b}"), "a"), "b");
}

#[test]
fn reads_block_scalars() {
    let doc = root(
        "lit: |\n  one\n   two\n\n  three\n\nfold: >\n  one\n  two\n\n  three\n    kept\n  four\nstrip: |-\n  x\n\nkeep: |+\n  x\n\nind: |2\n   x\nempty: |\nend: 1\n",
    );
    assert_eq!(text(&doc, "lit"), "one\n two\n\nthree\n");
    assert_eq!(text(&doc, "fold"), "one two\nthree\n  kept\nfour\n");
    assert_eq!(text(&doc, "strip"), "x");
    assert_eq!(text(&doc, "keep"), "x\n\n");
    assert_eq!(text(&doc, "ind"), " x\n");
    assert_eq!(text(&doc, "empty"), "");
    assert_eq!(text(&doc, "end"), "1");
    let listed = root("a:\n  - |\n    text\n  - b\n");
    assert_eq!(strings(get(&listed, "a")), ["text\n", "b"]);
}

#[test]
fn rejects_what_files_may_not_use() {
    assert_eq!(kind("a: &x 1\n"), ErrorKind::Unsupported);
    assert_eq!(kind("a: *x\n"), ErrorKind::Unsupported);
    assert_eq!(kind("a: !tag 1\n"), ErrorKind::Unsupported);
    assert_eq!(kind("%YAML 1.2\n---\na: 1\n"), ErrorKind::Unsupported);
    assert_eq!(kind("? a\n: 1\n"), ErrorKind::Unsupported);
    assert_eq!(kind("a: 1\n---\nb: 2\n"), ErrorKind::MultipleDocuments);
    assert_eq!(kind("a: 1\n...\nb: 2\n"), ErrorKind::MultipleDocuments);
    assert_eq!(kind("a: 1\na: 2\n"), ErrorKind::DuplicateKey);
    assert_eq!(kind("[a]: 1\n"), ErrorKind::NonScalarKey);
    assert_eq!(kind("- a\n"), ErrorKind::RootNotMap);
    assert_eq!(kind("text\n"), ErrorKind::RootNotMap);
    assert_eq!(kind(""), ErrorKind::RootNotMap);
    assert_eq!(kind("a:\n\tb: 2\n"), ErrorKind::Indentation);
    assert_eq!(kind("a: 1\n  b: 2\n"), ErrorKind::Syntax);
    assert_eq!(kind("a: b: c\n"), ErrorKind::Syntax);
    assert_eq!(kind("a: - b\n"), ErrorKind::Syntax);
    assert!(root("a: 1\n...\n").map().is_ok());
}

#[test]
fn checks_encoding_and_limits() {
    assert_eq!(text(&root("\u{feff}a: b\r\nc: d\r\n"), "c"), "d");
    let error = parse(b"a: 1\nb: \0\n").unwrap_err();
    assert_eq!(
        (error.line, error.column, error.kind),
        (2, 4, ErrorKind::NullByte)
    );
    assert_eq!(parse(b"a: \xff\n").unwrap_err().kind, ErrorKind::NotUtf8);
    let mut deep = "a: ".to_string();
    deep.push_str(&"[".repeat(DEPTH_MAX + 1));
    deep.push_str(&"]".repeat(DEPTH_MAX + 1));
    assert_eq!(kind(&deep), ErrorKind::TooDeep);
    let mut shallow = "a: ".to_string();
    shallow.push_str(&"[".repeat(DEPTH_MAX - 1));
    shallow.push_str(&"]".repeat(DEPTH_MAX - 1));
    assert!(parse(shallow.as_bytes()).is_ok());
    let big = vec![b' '; SIZE_MAX + 1];
    assert_eq!(parse(&big).unwrap_err().kind, ErrorKind::TooLarge);
}

#[test]
fn reports_where_errors_are() {
    let error = parse(b"a: 1\nb:\n  c: 1\n  c: 2\n").unwrap_err();
    assert_eq!((error.line, error.column), (4, 3));
    let doc = root("a: 1\nb: [x]\n");
    let b = get(&doc, "b");
    assert_eq!((b.line, b.column), (2, 4));
    let unknown: std::vec::Vec<_> = doc.map().unwrap().unknown(&["a"]).map(|e| e.line).collect();
    assert_eq!(unknown, [2]);
}
