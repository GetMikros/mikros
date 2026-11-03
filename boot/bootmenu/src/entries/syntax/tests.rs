use super::*;

fn syntax_error(result: Result<impl core::fmt::Debug, Error>) -> usize {
    match result {
        Err(Error {
            line: Some(line),
            kind: ErrorKind::Syntax(_),
        }) => line,
        other => panic!("expected a syntax error, got {other:?}"),
    }
}

#[test]
fn splits_content_lines() {
    let text = "a: 1\r\n\n   \n# comment\n  # indented comment\n\t# tab comment\n  b: 2\n";
    let lines = content_lines(text).unwrap();
    let summary: Vec<_> = lines.iter().map(|l| (l.number, l.indent, l.text)).collect();
    assert_eq!(summary, [(1, 0, "a: 1"), (7, 2, "b: 2")]);
}

#[test]
fn rejects_tab_indentation() {
    let err = content_lines("a: 1\n\tb: 2\n").unwrap_err();
    assert_eq!(err, error(2, ErrorKind::Tab));
}

#[test]
fn splits_pairs() {
    let p = pair("key: value # note", 3).unwrap();
    assert_eq!((p.key, p.value, p.line), ("key", Some("value # note"), 3));
    assert_eq!(pair("key:", 1).unwrap().value, None);
    assert_eq!(pair("key:   # only a comment", 1).unwrap().value, None);
    assert_eq!(pair("key: \t# after a tab", 1).unwrap().value, None);
    assert_eq!(pair("path: C:\\x: y", 1).unwrap().value, Some("C:\\x: y"));
    assert_eq!(syntax_error(pair("no colon here", 4)), 4);
    assert_eq!(syntax_error(pair("key:value", 1)), 1);
    assert_eq!(syntax_error(pair("\"key\": 1", 1)), 1);
    assert_eq!(syntax_error(pair("[key]: 1", 1)), 1);
}

#[test]
fn reads_plain_values() {
    assert_eq!(value("one two", 1).unwrap(), "one two");
    assert_eq!(value("one   # comment", 1).unwrap(), "one");
    assert_eq!(value("one\t# comment", 1).unwrap(), "one");
    assert_eq!(value("one \t # comment", 1).unwrap(), "one");
    assert_eq!(value("C# and x#1", 1).unwrap(), "C# and x#1");
    assert_eq!(value("a\tb\t", 1).unwrap(), "a\tb");
    assert_eq!(value("-5", 1).unwrap(), "-5");
    for bad in [
        "[1]", "{a: 1}", "*ref", "&anchor", "!tag", "|", ">", "- x", "a: b", "a:",
    ] {
        assert_eq!(syntax_error(value(bad, 2)), 2, "{bad}");
    }
}

#[test]
fn reads_double_quoted_values() {
    let text = r#""a\nb\tc\"d\\e\/f#g" # comment"#;
    assert_eq!(value(text, 1).unwrap(), "a\nb\tc\"d\\e/f#g");
    assert_eq!(value("\"\"", 1).unwrap(), "");
    assert_eq!(value("\"a\"\t# comment", 1).unwrap(), "a");
    assert_eq!(syntax_error(value("\"open", 1)), 1);
    assert_eq!(syntax_error(value(r#""\x""#, 1)), 1);
    assert_eq!(syntax_error(value("\"a\" b", 1)), 1);
    // A comment starts only after a space or a tab.
    assert_eq!(syntax_error(value("\"a\"# comment", 1)), 1);
    assert_eq!(syntax_error(value("\"a\" b # comment", 1)), 1);
}

#[test]
fn reads_single_quoted_values() {
    assert_eq!(value("'it''s # here'", 1).unwrap(), "it's # here");
    assert_eq!(value("'a\\n'", 1).unwrap(), "a\\n");
    assert_eq!(value("'a'\t# comment", 1).unwrap(), "a");
    assert_eq!(syntax_error(value("'open", 1)), 1);
    assert_eq!(syntax_error(value("'a' b", 1)), 1);
}

#[test]
fn recognizes_list_items() {
    assert!(is_item("- id: a"));
    assert!(is_item("-"));
    assert!(!is_item("-5"));
    assert!(!is_item("id: a"));
}
