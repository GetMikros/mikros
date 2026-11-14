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

fn expr(source: &str) -> Expr {
    match one(&format!("let x = {source}")) {
        StatementKind::Let { value, .. } => value,
        other => panic!("{other:?}"),
    }
}

fn int(n: i64) -> Expr {
    Expr::Literal(Literal::Int(n))
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
    assert_eq!(
        expr("1 +\n 2"),
        Expr::Binary(Binary::Add, Box::new(int(1)), Box::new(int(2)))
    );
    assert!(failure("echo \"open").incomplete);
    // A `\` at the end asks for the next line, also after a word.
    assert!(failure("echo a \\\n").incomplete);
    assert!(failure("echo a\\\n").incomplete);
    assert!(failure("echo a \\\r\n").incomplete);
    assert!(failure("ls |").incomplete);
    assert!(failure("if true {").incomplete);
    assert!(!failure("echo }").incomplete);
}

#[test]
fn keeps_brackets_after_text_in_the_word() {
    let s = simple("ls file[12].txt *.[ch] [a b]");
    assert_eq!(s.words[1], bare("file[12].txt"));
    assert_eq!(s.words[2], bare("*.[ch]"));
    assert!(
        matches!(&s.words[3], Argument::Word(w) if matches!(w.parts[0], WordPart::Expr(Expr::List(_))))
    );
    assert!(failure("ls a[b").incomplete);
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
    // `{` does not interpolate in a bare word: it ends it.
    assert_eq!(simple("echo a{b}").words.len(), 3);
}

#[test]
fn reads_double_quoted_strings() {
    let Expr::String(parts) = expr(r#""a\tb {1 + 2} \{x\} \u{e9}""#) else {
        panic!()
    };
    assert_eq!(parts[0], StringPart::Text("a\tb ".into()));
    assert!(matches!(
        parts[1],
        StringPart::Expr(Expr::Binary(Binary::Add, ..))
    ));
    assert_eq!(parts[2], StringPart::Text(" {x} é".into()));
    let Expr::String(parts) = expr(r#""a $b.c... \$d""#) else {
        panic!()
    };
    let StringPart::Expr(Expr::Variable(v)) = &parts[1] else {
        panic!()
    };
    assert_eq!((v.name.as_str(), v.spread), ("b", false));
    assert_eq!(v.path, [Access::Field("c".into())]);
    assert_eq!(parts[2], StringPart::Text("... $d".into()));
    assert_eq!(failure(r#"let x = "\q""#).message, "`\\q` is not an escape");
    assert_eq!(
        expr("'a\\n{b}'"),
        Expr::Literal(Literal::String("a\\n{b}".into()))
    );
}

#[test]
fn reads_let_assignments_and_environment() {
    assert!(matches!(one("let n = 3"), StatementKind::Let { name, .. } if name == "n"));
    assert!(matches!(
        one("n += 1"),
        StatementKind::Assign { target: Variable { name, environment: false, .. }, op: AssignOp::Append, .. } if name == "n"
    ));
    assert!(matches!(
        one("env.PATH = null"),
        StatementKind::Assign { target: Variable { name, environment: true, .. }, op: AssignOp::Set, .. } if name == "PATH"
    ));
    let s = simple("LANG=C TZ=UTC date");
    assert_eq!(s.environment.len(), 2);
    assert_eq!(s.words, [bare("date")]);
    // `==` is a comparison, not an assignment.
    assert!(matches!(one("a == b"), StatementKind::Pipeline(_)));
}

#[test]
fn reads_paths_and_removal_in_assignments() {
    let StatementKind::Assign { target, op, .. } = one("cfg.servers.0.port -= 80") else {
        panic!()
    };
    assert_eq!(op, AssignOp::Remove);
    assert_eq!(target.name, "cfg");
    assert_eq!(
        target.path,
        [
            Access::Field("servers".into()),
            Access::Item(0),
            Access::Field("port".into())
        ]
    );
    // A word with dots that no assignment operator follows is a command.
    assert!(matches!(one("run.sh a.b"), StatementKind::Pipeline(_)));
}

#[test]
fn reads_right_sides_as_statements() {
    let value = |source: &str| match one(source) {
        StatementKind::Let { value, .. } => value,
        StatementKind::Assign { value, .. } => value,
        StatementKind::Return(Some(value)) => value,
        other => panic!("{other:?}"),
    };
    assert!(matches!(value("let files = ls -l"), Expr::Pipeline(p) if p.commands.len() == 1));
    assert!(matches!(value("x = $l | len"), Expr::Pipeline(p) if p.commands.len() == 2));
    assert!(matches!(value("return echo a"), Expr::Pipeline(_)));
    assert!(matches!(value("let x = -2"), Expr::Unary(Unary::Negate, _)));
    assert!(matches!(
        value("let x = 1 + 2"),
        Expr::Binary(Binary::Add, ..)
    ));
}

#[test]
fn reads_quoted_field_names() {
    let Expr::Variable(v) = expr("$foo.\"yoo sup\".0.'a'") else {
        panic!()
    };
    assert_eq!(
        v.path,
        [
            Access::Field("yoo sup".into()),
            Access::Item(0),
            Access::Field("a".into())
        ]
    );
    assert_eq!(
        failure("print $foo.\"{1}\"").message,
        "a field's name cannot interpolate"
    );
}

#[test]
fn reads_home_folders_at_the_start_of_bare_words() {
    let s = simple("cat ~/a ~bob a~ ~$x > ~/out");
    let first = |a: &Argument| match a {
        Argument::Word(w) => w.parts[0].clone(),
        Argument::Block(_) => panic!(),
    };
    assert_eq!(first(&s.words[1]), WordPart::Home(None));
    assert_eq!(first(&s.words[2]), WordPart::Home(Some("bob".into())));
    assert_eq!(first(&s.words[3]), WordPart::Bare("a~".into()));
    assert_eq!(first(&s.words[4]), WordPart::Bare("~".into()));
    assert_eq!(s.redirections[0].target.parts[0], WordPart::Home(None));
}

#[test]
fn reads_delete() {
    let StatementKind::Delete(v) = one("delete $env.PATH.1") else {
        panic!()
    };
    assert_eq!((v.environment, v.name.as_str()), (true, "PATH"));
    assert_eq!(v.path, [Access::Item(1)]);
    assert!(matches!(one("delete env.EDITOR"), StatementKind::Delete(v) if v.environment));
    assert_eq!(
        failure("delete x").message,
        "`delete` takes `$name` or `env.NAME`"
    );
}

#[test]
fn reads_operators_by_precedence() {
    // 1 + 2 * 3 == 7 and not false
    let e = expr("1 + 2 * 3 == 7 and not false");
    let Expr::Binary(Binary::And, left, right) = e else {
        panic!()
    };
    assert!(matches!(*right, Expr::Unary(Unary::Not, _)));
    let Expr::Binary(Binary::Equal, sum, _) = *left else {
        panic!()
    };
    let Expr::Binary(Binary::Add, _, product) = *sum else {
        panic!()
    };
    assert!(matches!(*product, Expr::Binary(Binary::Multiply, ..)));
    assert!(matches!(
        expr("$a ?? 1 + 2"),
        Expr::Binary(Binary::Default, ..)
    ));
    assert!(matches!(
        expr("$a =~ 'x' or $b"),
        Expr::Binary(Binary::Or, ..)
    ));
    assert!(matches!(expr("-$a"), Expr::Unary(Unary::Negate, _)));
    assert!(
        matches!(expr("8 - 2 - 1"), Expr::Binary(Binary::Subtract, l, _) if matches!(*l, Expr::Binary(Binary::Subtract, ..)))
    );
}

#[test]
fn reads_numbers_sizes_durations_and_dates() {
    assert_eq!(expr("0x2a"), int(42));
    assert_eq!(expr("1_000"), int(1000));
    assert_eq!(expr("1.5"), Expr::Literal(Literal::Float(1.5)));
    assert_eq!(expr("2e3"), Expr::Literal(Literal::Float(2000.0)));
    assert_eq!(expr("10mb"), Expr::Literal(Literal::Size(10_000_000)));
    assert_eq!(expr("2kib"), Expr::Literal(Literal::Size(2048)));
    assert_eq!(
        expr("1.5s"),
        Expr::Literal(Literal::Duration(1_500_000_000))
    );
    assert_eq!(
        expr("1day"),
        Expr::Literal(Literal::Duration(86_400_000_000_000))
    );
    let Expr::Literal(Literal::Date(d)) = expr("2026-11-10") else {
        panic!()
    };
    assert_eq!((d.seconds, d.offset), (1_794_268_800, None));
    let Expr::Literal(Literal::Date(d)) = expr("2026-11-10T12:00:00+01:00") else {
        panic!()
    };
    assert_eq!(
        (d.seconds, d.offset),
        (1_794_268_800 + 11 * 3600, Some(3600))
    );
    let Expr::Literal(Literal::Date(d)) = expr("1970-01-01T00:00:00.5Z") else {
        panic!()
    };
    assert_eq!((d.seconds, d.nanos), (0, 500_000_000));
    assert!(failure("let x = 3parsecs").message.contains("parsecs"));
    assert!(failure("let x = 2026-02-30").message.contains("date"));
}

#[test]
fn reads_lists_records_access_and_blocks() {
    let Expr::List(items) = expr("[a 'b' 3]") else {
        panic!()
    };
    assert!(matches!(items[0], Expr::Words(_)));
    assert_eq!(items[2], int(3));
    let Expr::Record(fields) = expr("{name: \"x\", size: 3}") else {
        panic!()
    };
    assert_eq!(fields[1], ("size".into(), int(3)));
    assert!(matches!(expr("[1 2].0"), Expr::Access(_, Access::Item(0))));
    assert!(matches!(expr("{ $it + 1 }"), Expr::Block(_)));
    let s = simple("each { $it + 1 }");
    assert!(matches!(s.words[1], Argument::Block(_)));
}

#[test]
fn reads_parentheses_as_expression_or_command() {
    assert!(matches!(expr("(1 + 2)"), Expr::Binary(..)));
    assert!(matches!(expr("(ls | lines)"), Expr::Pipeline(_)));
    assert!(matches!(one("(1 + 2)"), StatementKind::Expr(_)));
    // A statement that starts like an expression is one.
    assert!(matches!(
        one("$x + 1"),
        StatementKind::Expr(Expr::Binary(..))
    ));
    assert!(matches!(one("'a' + 'b'"), StatementKind::Expr(_)));
    assert!(matches!(
        one("[1 2] | each { $it }"),
        StatementKind::Pipeline(_)
    ));
    // Fields and items of parentheses, in arguments too.
    let s = simple("print (f).status");
    assert!(
        matches!(&s.words[1], Argument::Word(w) if matches!(w.parts[0], WordPart::Expr(Expr::Access(..))))
    );
    let StatementKind::Pipeline(p) = one("(ls) | lines") else {
        panic!()
    };
    assert!(matches!(p.commands[0], Command::Expr(_)));
    let StatementKind::Let { value, .. } = one("let n = (\"abc\" | wc -c)") else {
        panic!()
    };
    let Expr::Pipeline(p) = value else { panic!() };
    assert!(matches!(p.commands[0], Command::Expr(Expr::String(_))));
    assert!(matches!(&p.commands[1], Command::Simple(s) if s.words.len() == 2));
}

#[test]
fn reads_conditions_as_expression_or_command() {
    let StatementKind::If {
        condition,
        otherwise,
        ..
    } = one("if $n > 1 { a } else if grep -q x f { b } else { c }")
    else {
        panic!()
    };
    assert!(matches!(condition, Condition::Expr(_)));
    let StatementKind::If { condition, .. } = &otherwise.unwrap().statements[0].kind else {
        panic!()
    };
    assert!(matches!(condition, Condition::Command(_)));
    assert!(matches!(
        one("while test -f lock { sleep 1 }"),
        StatementKind::While {
            condition: Condition::Command(_),
            ..
        }
    ));
}

#[test]
fn reads_loops_functions_params_try_and_use() {
    let StatementKind::For { name, items, body } = one("for f in *.txt b {\n cat $f\n}") else {
        panic!()
    };
    assert_eq!(name, "f");
    assert!(matches!(items, Expr::Words(w) if w.len() == 2));
    assert_eq!(body.statements.len(), 1);
    assert!(matches!(
        one("for x in $list { echo $x }"),
        StatementKind::For {
            items: Expr::Variable(_),
            ..
        }
    ));
    let StatementKind::Fn(f) = one("fn greet(name, --loud = false) { return \"hi\" }") else {
        panic!()
    };
    assert_eq!(f.arguments, ["name"]);
    assert_eq!(f.flags[0].0, "loud");
    assert!(matches!(
        f.body.statements[0].kind,
        StatementKind::Return(Some(_))
    ));
    let StatementKind::Param(p) = one("param --count: int = 3") else {
        panic!()
    };
    assert_eq!((p.name.as_str(), p.kind), ("count", ParamType::Int));
    assert!(failure("param --x: thing").message.contains("thing"));
    assert!(matches!(
        one("try { rm x } catch e { echo $e.message }"),
        StatementKind::Try { .. }
    ));
    assert!(matches!(one("use lib/util.msh"), StatementKind::Use(_)));
    // `(command)?` propagates; a lone `?` is a word.
    let StatementKind::Expr(Expr::Propagate(inner)) = one("(rm x)?") else {
        panic!()
    };
    assert!(matches!(*inner, Expr::Pipeline(_)));
    assert_eq!(simple("ls ?").words[1], bare("?"));
}

#[test]
fn reads_match_and_patterns() {
    let source = "match $v {\n 0 | 1 => { a }\n 2..10 => { b }\n [first ...rest] => { c }\n {name: n} if $n != '' => { d }\n _ => { e }\n}";
    let StatementKind::Match { arms, .. } = one(source) else {
        panic!()
    };
    assert_eq!(arms.len(), 5);
    assert!(matches!(&arms[0].pattern, Pattern::Either(p) if p.len() == 2));
    assert!(matches!(
        arms[1].pattern,
        Pattern::Range(Literal::Int(2), Literal::Int(10))
    ));
    assert!(
        matches!(&arms[2].pattern, Pattern::List { items, rest: Some(r) } if items.len() == 1 && r == "rest")
    );
    assert!(matches!(&arms[3].pattern, Pattern::Record(f) if f[0].1 == Pattern::Bind("n".into())));
    assert!(arms[3].guard.is_some());
    assert_eq!(arms[4].pattern, Pattern::Any);
    // `match` after `|` is a command.
    let StatementKind::Pipeline(p) = one("ls | match 'a(.)'") else {
        panic!()
    };
    assert_eq!(p.commands.len(), 2);
}

#[test]
fn names_the_line_of_an_error() {
    let e = failure("ls\npwd\nlet = 3");
    assert_eq!(e.line, 3);
    let lines: Vec<usize> = parse("a\n\nb; c\n")
        .unwrap()
        .statements
        .iter()
        .map(|s| s.line)
        .collect();
    assert_eq!(lines, [1, 3, 3]);
}
