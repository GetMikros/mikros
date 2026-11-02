use super::*;

fn err(text: &str) -> Error {
    parse(text.as_bytes()).expect_err(text)
}

fn root(text: &str) -> Value {
    parse(text.as_bytes()).unwrap_or_else(|e| panic!("{text:?}: {e}"))
}

#[test]
fn reads_block_and_flow_collections_and_all_scalar_styles() {
    let text = "---\r\n# comment\r\nplain: a b\nsingle: 'it''s'\ndouble: \"x\\ty\"\n\
                literal: |\n  one\n  two\nfolded: >\n  one\n  two\n\
                list: [1, '2', {k: v}]\nblock:\n  - x\n  - y: z\n";
    let r = root(text);
    let mut m = r.as_map().unwrap();
    assert_eq!(m.require("plain").unwrap().as_str().unwrap(), "a b");
    assert_eq!(m.require("single").unwrap().as_str().unwrap(), "it's");
    assert_eq!(m.require("double").unwrap().as_str().unwrap(), "x\ty");
    assert_eq!(
        m.require("literal").unwrap().as_str().unwrap(),
        "one\ntwo\n"
    );
    assert_eq!(m.require("folded").unwrap().as_str().unwrap(), "one two\n");
    let list = m.require("list").unwrap().as_list().unwrap();
    assert_eq!(list[0].as_int(0, 9).unwrap(), 1);
    assert!(list[1].as_int(0, 9).is_err());
    assert_eq!(
        list[2]
            .as_map()
            .unwrap()
            .require("k")
            .unwrap()
            .as_str()
            .unwrap(),
        "v"
    );
    let block = m.require("block").unwrap().as_list().unwrap();
    assert_eq!(block.len(), 2);
    assert!(m.unknown().is_empty());
}

#[test]
fn ignores_a_byte_order_mark() {
    let r = parse("\u{FEFF}a: 1\n".as_bytes()).unwrap();
    assert_eq!(
        r.as_map()
            .unwrap()
            .require("a")
            .unwrap()
            .as_int(0, 1)
            .unwrap(),
        1
    );
}

#[test]
fn takes_types_from_the_reader_not_the_text() {
    let r = root("no: no\nmode: 0755\nversion: 1.10\nt: true\nq: 'true'\n");
    let mut m = r.as_map().unwrap();
    assert_eq!(m.require("no").unwrap().as_str().unwrap(), "no");
    assert!(m.require("no").unwrap().as_bool().is_err());
    assert_eq!(m.require("mode").unwrap().as_str().unwrap(), "0755");
    assert_eq!(m.require("mode").unwrap().as_int(0, 1000).unwrap(), 755);
    assert_eq!(m.require("version").unwrap().as_str().unwrap(), "1.10");
    assert!(m.require("version").unwrap().as_int(0, 100).is_err());
    assert!(m.require("t").unwrap().as_bool().unwrap());
    assert_eq!(m.require("t").unwrap().as_str().unwrap(), "true");
    assert!(m.require("q").unwrap().as_bool().is_err());
}

#[test]
fn reads_integers_in_range() {
    let r = root("a: -12\nb: 0x1F\nc: 0X1F\nd: -0x1\ne: 1_000\nf: 300\ng: '5'\nh: +5\n");
    let mut m = r.as_map().unwrap();
    assert_eq!(m.require("a").unwrap().as_int(-100, 100).unwrap(), -12);
    assert_eq!(m.require("b").unwrap().as_int(0, 255).unwrap(), 31);
    for key in ["c", "d", "e", "g", "h"] {
        assert!(
            m.require(key).unwrap().as_int(-1000, 1000).is_err(),
            "{key}"
        );
    }
    let f = m.require("f").unwrap();
    assert_eq!(
        f.as_int(0, 255).unwrap_err().message,
        "expected an integer from 0 to 255"
    );
}

#[test]
fn treats_null_and_empty_values_as_absent() {
    let r = root("a: null\nb: ~\nc:\nd: 'null'\n");
    let mut m = r.as_map().unwrap();
    assert!(m.get("a").is_none());
    assert!(m.get("b").is_none());
    assert!(m.get("c").is_none());
    assert_eq!(m.get("d").unwrap().as_str().unwrap(), "null");
    assert_eq!(m.require("a").unwrap_err().message, "missing key `a`");
}

#[test]
fn reports_unread_keys() {
    let r = root("known: 1\nextra: [1, 2]\n");
    let mut m = r.as_map().unwrap();
    m.require("known").unwrap();
    assert_eq!(
        m.unknown(),
        [("extra".to_owned(), Position { line: 2, column: 1 })]
    );
}

#[test]
fn rejects_what_mikros_files_may_not_use() {
    let cases = [
        ("a: &x 1\nb: *x\n", 1, "anchors are not allowed"),
        ("a: !!str 1\n", 1, "tags are not allowed"),
        ("%YAML 1.2\n---\na: 1\n", 1, "directives are not allowed"),
        ("? a\n: 1\n", 1, "explicit keys are not allowed"),
        ("a: 1\n---\nb: 2\n", 2, "only one document is allowed"),
        ("a: 1\na: 2\n", 2, "the key `a` appears twice"),
        ("'a': 1\na: 2\n", 2, "the key `a` appears twice"),
        ("[1]: x\n", 1, "keys must be scalars"),
        ("- a\n", 1, "the root must be a map"),
        ("just text\n", 1, "the root must be a map"),
        ("", 1, "the root must be a map"),
        ("a: \0\n", 1, "the file contains a null byte"),
    ];
    for (text, line, message) in cases {
        let e = err(text);
        assert_eq!((e.line, e.message.as_str()), (line, message), "{text:?}");
    }
    // A plain scalar may start with `?`.
    assert!(parse(b"?a: 1\n").is_ok());
}

#[test]
fn rejects_files_that_break_the_limits() {
    let e = parse(b"a: \xFF\n").unwrap_err();
    assert_eq!(
        (e.line, e.column, e.message.as_str()),
        (1, 4, "the file is not UTF-8")
    );
    let deep = |n: usize| format!("a: {}{}\n", "[".repeat(n), "]".repeat(n));
    assert!(parse(deep(31).as_bytes()).is_ok());
    assert_eq!(
        err(&deep(32)).message,
        "collections are nested more than 32 deep"
    );
    let big = vec![b' '; MAX_SIZE + 1];
    assert_eq!(
        parse(&big).unwrap_err().message,
        "the file is larger than 16777216 bytes"
    );
}

#[test]
fn reports_syntax_errors_with_their_position() {
    let e = err("a: [1, 2\n");
    assert_eq!(e.line, 2);
    let e = err("a: 'open\n");
    assert!(e.line >= 1);
}
