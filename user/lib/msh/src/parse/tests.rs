use super::*;

fn statements(source: &str) -> Vec<StatementKind> {
    parse(source)
        .unwrap_or_else(|e| panic!("{source:?}: {e}"))
        .statements
        .into_iter()
        .map(|s| s.kind)
        .collect()
}

fn one(source: &str) -> StatementKind {
    let mut all = statements(source);
    assert_eq!(all.len(), 1, "{source:?}");
    all.pop().unwrap()
}

fn simple(source: &str) -> Simple {
    match one(source) {
        StatementKind::Pipeline(p) if p.commands.len() == 1 => {
            match p.commands.into_iter().next() {
                Some(Command::Simple(s)) => s,
                other => panic!("{other:?}"),
            }
        }
        other => panic!("{other:?}"),
    }
}

fn bare(text: &str) -> Argument {
    Argument::Word(Word {
        parts: vec![WordPart::Bare(text.into())],
    })
}

fn failure(source: &str) -> Error {
    parse(source).expect_err(source)
}

#[test]
fn splits_statements_and_words() {
    assert_eq!(statements("ls -l; pwd\n\necho a#b # comment").len(), 3);
    let s = simple("cp a.txt 'b c'/x \"d\"");
    assert_eq!(s.words[0], bare("cp"));
    assert_eq!(s.words[1], bare("a.txt"));
    assert_eq!(
        s.words[2],
        Argument::Word(Word {
            parts: vec![WordPart::Quoted("b c".into()), WordPart::Bare("/x".into())],
        })
    );
    // `#` starts a comment only at the start of a word.
    assert_eq!(simple("echo a#b").words[1], bare("a#b"));
    // `=` is not special in an argument.
    assert_eq!(simple("echo a=b").words[1], bare("a=b"));
}

#[test]
fn reads_pipelines_redirections_and_background() {
    let StatementKind::Pipeline(p) = one("cat < in | sort -r > out 2>&1 &") else {
        panic!()
    };
    assert!(p.background);
    assert_eq!(p.commands.len(), 2);
    let Command::Simple(sort) = &p.commands[1] else {
        panic!()
    };
    assert_eq!(sort.redirections.len(), 2);
    assert_eq!(sort.redirections[0].kind, RedirectionKind::Output);
    assert_eq!(sort.redirections[1].kind, RedirectionKind::ErrorToOutput);
    let s = simple("make 2> errors >> log >&2");
    let kinds: Vec<_> = s.redirections.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        [
            RedirectionKind::Error,
            RedirectionKind::Append,
            RedirectionKind::OutputToError
        ]
    );
}

#[test]
fn continues_lines() {
    let StatementKind::Pipeline(p) = one("ls |\n  sort") else {
        panic!()
    };
    assert_eq!(p.commands.len(), 2);
    assert_eq!(simple("echo a \\\n b").words.len(), 3);
    assert!(failure("echo \"open").incomplete);
    assert!(failure("ls |").incomplete);
    assert!(!failure("echo }").incomplete);
}

#[test]
fn reads_variables_in_words() {
    let s = simple("cat $dir/file $list... $rec.name.0");
    let Argument::Word(w) = &s.words[1] else {
        panic!()
    };
    assert!(matches!(&w.parts[0], WordPart::Variable(v) if v.name == "dir"));
    assert_eq!(w.parts[1], WordPart::Bare("/file".into()));
    let Argument::Word(w) = &s.words[2] else {
        panic!()
    };
    assert!(matches!(&w.parts[0], WordPart::Variable(v) if v.spread));
    let Argument::Word(w) = &s.words[3] else {
        panic!()
    };
    let WordPart::Variable(v) = &w.parts[0] else {
        panic!()
    };
    assert_eq!(v.path, [Access::Field("name".into()), Access::Item(0)]);
}

#[test]
fn names_the_line_of_an_error() {
    let e = failure("ls\npwd\necho }");
    assert_eq!(e.line, 3);
    let lines: Vec<usize> = parse("a\n\nb; c\n")
        .unwrap()
        .statements
        .iter()
        .map(|s| s.line)
        .collect();
    assert_eq!(lines, [1, 3, 3]);
}
