use super::*;
use crate::jobs::Ended;
use crate::parse::parse;
use std::collections::BTreeMap;

/// A program: its arguments and input give its exit status and output.
type Behaviour = fn(&[String], &[u8]) -> (i32, Vec<u8>);

/// A system in memory.
#[derive(Default)]
pub(crate) struct Fake {
    cwd: String,
    folders: BTreeMap<String, Vec<(String, bool)>>,
    files: BTreeMap<String, String>,
    programs: BTreeMap<String, Behaviour>,
    output: Vec<u8>,
    errors: Vec<u8>,
    runs: Vec<Run>,
    resumed: Vec<(String, bool)>,
    killed: Vec<(String, i32)>,
}

impl glob::Folders for Fake {
    fn entries(&self, path: &str) -> Option<Vec<(String, bool)>> {
        self.folders.get(path).cloned()
    }
}

impl System for Fake {
    fn current_dir(&self) -> String {
        self.cwd.clone()
    }
    fn set_current_dir(&mut self, path: &str) -> Result<(), String> {
        match self.folders.contains_key(path) {
            true => {
                self.cwd = path.into();
                Ok(())
            }
            false => Err(format!("{path}: no such folder")),
        }
    }
    fn write_file(&mut self, path: &str, bytes: &[u8], append: bool) -> Result<(), String> {
        let file = self.files.entry(path.into()).or_default();
        if !append {
            file.clear();
        }
        file.push_str(&String::from_utf8_lossy(bytes));
        Ok(())
    }
    fn read_file(&self, path: &str) -> Result<String, String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("{path}: not found"))
    }
    fn home_of(&self, name: &str) -> Option<String> {
        (name == "bob").then(|| "/Users/bob".into())
    }
    fn executable(&self, path: &str) -> Option<bool> {
        match self.programs.contains_key(path) {
            true => Some(true),
            false => self.files.contains_key(path).then_some(false),
        }
    }
    fn run(&mut self, run: Run) -> Result<Ran, String> {
        self.runs.push(run.clone());
        let mut data = match run.input {
            Input::Inherit => Vec::new(),
            Input::Bytes(b) => b,
        };
        let mut ended = Vec::new();
        for program in &run.programs {
            let behaviour = self.programs[&program.path];
            let (status, output) = behaviour(&program.args, &data);
            ended.push(Ended::Exited(status));
            data = output;
        }
        if run.capture {
            return Ok(Ran {
                ended,
                output: data,
            });
        }
        self.output.extend(data);
        Ok(Ran {
            ended,
            output: Vec::new(),
        })
    }
    fn jobs(&mut self) -> Vec<String> {
        vec![format!("[1]+  {:<24}sleep 9", "Stopped")]
    }
    fn resume(&mut self, id: &str, foreground: bool) -> Result<Ran, String> {
        self.resumed.push((id.into(), foreground));
        Ok(Ran {
            ended: vec![Ended::Exited(if foreground { 3 } else { 0 })],
            output: Vec::new(),
        })
    }
    fn kill(&mut self, target: &str, signal: i32) -> Result<(), String> {
        self.killed.push((target.into(), signal));
        Ok(())
    }
    fn exec(&mut self, program: Program) -> String {
        format!("{} cannot replace a test", program.path)
    }
    fn write(&mut self, stream: Stream, bytes: &[u8]) {
        match stream {
            Stream::Output => self.output.extend_from_slice(bytes),
            Stream::Error => self.errors.extend_from_slice(bytes),
        }
    }
}

pub(crate) fn shell() -> Shell<Fake> {
    let mut fake = Fake {
        cwd: "/home/ada".into(),
        ..Fake::default()
    };
    fake.folders.insert("/home/ada".into(), vec![]);
    fake.folders.insert("/tmp".into(), vec![]);
    fake.folders.insert(
        ".".into(),
        vec![
            ("a.txt".into(), false),
            ("b.txt".into(), false),
            ("c.md".into(), false),
        ],
    );
    let programs: [(&str, Behaviour); 4] = [
        ("/bin/echo", |args, _| {
            (0, format!("{}\n", args[1..].join(" ")).into_bytes())
        }),
        ("/bin/cat", |_, input| (0, input.to_vec())),
        ("/bin/false", |_, _| (1, Vec::new())),
        ("/bin/shout", |_, input| {
            (
                0,
                String::from_utf8_lossy(input).to_uppercase().into_bytes(),
            )
        }),
    ];
    for (path, behaviour) in programs {
        fake.programs.insert(path.into(), behaviour);
    }
    Shell::new(
        fake,
        [
            ("PATH".into(), "/bin:/usr/bin".into()),
            ("HOME".into(), "/home/ada".into()),
        ],
    )
}

/// Runs `source` and gives what it wrote, or its error.
pub(crate) fn run(shell: &mut Shell<Fake>, source: &str) -> Result<String, Flow> {
    let block = parse(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
    let before = shell.system.output.len();
    shell.top(&block)?;
    Ok(String::from_utf8_lossy(&shell.system.output[before..]).into_owned())
}

pub(crate) fn ok(source: &str) -> String {
    run(&mut shell(), source).unwrap_or_else(|e| panic!("{source:?}: {e:?}"))
}

fn error(source: &str) -> Failure {
    match run(&mut shell(), source) {
        Err(Flow::Error(f)) => f,
        other => panic!("{source:?}: {other:?}"),
    }
}

#[test]
fn sets_reads_and_changes_variables() {
    assert_eq!(ok("let n = 2\nn = $n * 3\nprint $n"), "6\n");
    assert_eq!(ok("let l = [1]\nl += 2\nprint $l"), "1\n2\n");
    assert_eq!(ok("let s = 'a'\ns += 'b'\nprint $s"), "ab\n");
    assert_eq!(error("print $nope").message, "`$nope` is not set");
    assert!(error("nope = 1").message.contains("`let` creates it"));
    assert_eq!(ok("print ($nope ?? 'default')"), "default\n");
    assert_eq!(ok("let r = {a: {b: [7 8]}}\nprint $r.a.b.1"), "8\n");
}

#[test]
fn removes_from_lists_and_assigns_to_items_and_fields() {
    assert_eq!(ok("let l = [1 2 1 3]\nl -= 1\nprint $l"), "2\n3\n");
    assert_eq!(ok("let l = [a b]\nl.1 = 'x'\nprint $l"), "a\nx\n");
    let cfg = "let cfg = {servers: [{port: 1, tags: [a b]}]}\n";
    let out = |source: &str| ok(&format!("{cfg}{source}"));
    assert_eq!(
        out("cfg.servers.0.port = 80\nprint $cfg.servers.0.port"),
        "80\n"
    );
    assert_eq!(
        out("cfg.servers.0.tags += 'c'\ncfg.servers.0.tags -= 'a'\nprint $cfg.servers.0.tags"),
        "b\nc\n"
    );
    // `=` adds a field that a record lacks.
    assert_eq!(out("cfg.name = 'x'\nprint $cfg.name"), "x\n");
    let mut s = shell();
    run(&mut s, "env.PATH.0 = '/opt/bin'\nenv.PATH -= '/usr/bin'").unwrap();
    assert_eq!(run(&mut s, "print $env.PATH").unwrap(), "/opt/bin\n");
    let failure = |source: &str| error(&format!("{cfg}{source}")).message;
    assert_eq!(failure("cfg.servers.5 = 1"), "the list has no item 5");
    assert_eq!(failure("cfg.servers.0.port.x = 1"), "a int has no fields");
    assert_eq!(failure("cfg.a.b = 1"), "the record has no field `a`");
    assert_eq!(failure("cfg.a += 1"), "the record has no field `a`");
    assert_eq!(
        failure("let s = 'ab'\ns -= 'b'"),
        "`-=` removes from a list, not a string"
    );
    assert_eq!(
        failure("nope.a = 1"),
        "`$nope` is not set; `let` creates it"
    );
}

#[test]
fn deletes_variables_fields_and_items() {
    assert_eq!(ok("let x = 1\ndelete $x\nprint ($x ?? 'gone')"), "gone\n");
    assert_eq!(
        ok("let r = {a: 1, b: [1 2]}\ndelete $r.a\ndelete $r.b.0\nprint ($r | to json)"),
        "{\"b\":[2]}\n"
    );
    let mut s = shell();
    run(
        &mut s,
        "env.EDITOR = 'vi'\ndelete env.EDITOR\ndelete $env.PATH.1",
    )
    .unwrap();
    assert!(s.env("EDITOR").is_none());
    assert_eq!(run(&mut s, "print $env.PATH").unwrap(), "/bin\n");
    // Deleting what does not exist is an error.
    assert_eq!(error("delete $nope").message, "`$nope` is not set");
    assert_eq!(error("delete env.NOPE").message, "`$env.NOPE` is not set");
    assert_eq!(
        error("let r = {a: 1}\ndelete $r.c").message,
        "the record has no field `c`"
    );
    assert_eq!(
        error("let l = [1]\ndelete $l.3").message,
        "the list has no item 3"
    );
    assert_eq!(
        error("let n = 1\ndelete $n.0").message,
        "a int has no items"
    );
}

#[test]
fn reads_right_sides_as_statements() {
    let same = |plain: &str, parenthesized: &str| assert_eq!(ok(plain), ok(parenthesized));
    same(
        "let x = echo hi | shout\nprint $x",
        "let x = (echo hi | shout)\nprint $x",
    );
    same(
        "fn f() { return echo hi | shout }\nprint (f)",
        "fn f() { return (echo hi | shout) }\nprint (f)",
    );
    assert_eq!(ok("let n = [1 2 3] | len\nprint $n"), "3\n");
    assert_eq!(ok("let n = 0\nn = 'abc' | len\nprint $n"), "3\n");
    let mut s = shell();
    run(&mut s, "env.PATH = $env.PATH | where { $it != '/bin' }").unwrap();
    assert_eq!(run(&mut s, "print $env.PATH").unwrap(), "/usr/bin\n");
    // Expressions, negations among them, are still values.
    assert_eq!(ok("let x = 2 + 3 * 2\nprint $x"), "8\n");
    assert_eq!(ok("let a = 3\nlet x = -$a - 2\nprint $x"), "-5\n");
    assert_eq!(ok("let x = -2.5\nprint $x"), "-2.5\n");
}

#[test]
fn reads_and_changes_fields_with_quoted_names() {
    let foo = "let foo = {\"hello\": 'world', \"yoo sup\": 621}\n";
    let out = |source: &str| ok(&format!("{foo}{source}"));
    assert_eq!(out("print $foo.\"yoo sup\" $foo.'yoo sup'"), "621 621\n");
    assert_eq!(out("echo $foo.\"yoo sup\"/x"), "621/x\n");
    assert_eq!(out("print ($foo).'yoo sup'"), "621\n");
    assert_eq!(
        out("foo.\"yoo sup\" = 1\nfoo.'a b' = 2\nprint $foo.\"yoo sup\" $foo.\"a b\""),
        "1 2\n"
    );
    assert_eq!(
        out("delete $foo.\"yoo sup\"\nprint ($foo | to json)"),
        "{\"hello\":\"world\"}\n"
    );
    assert_eq!(ok("print ({\"hello\": 1} == {hello: 1})"), "true\n");
    // In a double-quoted string, a quote after a dot ends the string.
    assert_eq!(out("echo \"$foo.hello.\""), "world.\n");
}

#[test]
fn keeps_path_a_list_and_passes_the_environment() {
    let mut s = shell();
    run(
        &mut s,
        "env.PATH += '/opt/bin'\nenv.EDITOR = 'vi'\nLANG=C echo hi",
    )
    .unwrap();
    let program = &s.system.runs[0].programs[0];
    let env: BTreeMap<_, _> = program.environment.iter().cloned().collect();
    assert_eq!(env["PATH"], "/bin:/usr/bin:/opt/bin");
    assert_eq!(env["EDITOR"], "vi");
    assert_eq!(env["LANG"], "C");
    run(&mut s, "env.EDITOR = null").unwrap();
    assert!(s.env("EDITOR").is_none());
    assert_eq!(run(&mut s, "print $env.PATH.0").unwrap(), "/bin\n");
}

#[test]
fn interpolates_names_in_double_quotes_but_not_in_single_quotes() {
    let set = "let x = 'v'\nlet r = {name: 'n', items: [1 2]}\n";
    let out = |source: &str| ok(&format!("{set}{source}"));
    assert_eq!(out("echo \"a $x b\""), "a v b\n");
    assert_eq!(out("echo \"$r.name/$r.items.1 {$x}\""), "n/2 v\n");
    assert_eq!(out("echo \"$env.HOME\""), "/home/ada\n");
    // A `$` that no name follows is text, as `\$` is, and so is `...`.
    assert_eq!(
        out("echo \"\\$x $ costs $5 $x...\""),
        "$x $ costs $5 v...\n"
    );
    // The string is one argument, and gives its text.
    let mut s = shell();
    run(&mut s, "let l = [1 2]\necho \"$l\" $l...").unwrap();
    assert_eq!(s.system.runs[0].programs[0].args[1..], ["[1,2]", "1", "2"]);
    assert_eq!(ok("let n = 3\nprint (\"$n\" == '3')"), "true\n");
    assert_eq!(error("echo \"$nope\"").message, "`$nope` is not set");
    // Single quotes interpolate nothing.
    assert_eq!(out("echo '$x {$x}' a'$x'b"), "$x {$x} a$xb\n");
    assert_eq!(out("print '$x'"), "$x\n");
}

#[test]
fn sets_environment_words_for_one_command_and_assigns_without_one() {
    let mut s = shell();
    run(&mut s, "HELLO_WORLD=foobar env.EDITOR=vi LANG=C echo hi").unwrap();
    let program = &s.system.runs[0].programs[0];
    assert_eq!(program.args, ["echo", "hi"]);
    let env: BTreeMap<_, _> = program.environment.iter().cloned().collect();
    assert_eq!(
        [&env["HELLO_WORLD"], &env["EDITOR"], &env["LANG"]],
        ["foobar", "vi", "C"]
    );
    // The words change neither the shell's variables nor its environment.
    assert!(s.env("HELLO_WORLD").is_none() && s.env("EDITOR").is_none());
    assert!(run(&mut s, "print $HELLO_WORLD").is_err());
    // Without a command, they assign: a value that starts like an
    // expression keeps its kind, and any other is text.
    run(&mut s, "env.EDITOR=vi").unwrap();
    assert_eq!(s.env("EDITOR"), Some(&Value::String("vi".into())));
    let assigned = |source: &str| {
        let mut s = shell();
        run(&mut s, &format!("let y = 'v'\nlet x = 0\n{source}")).unwrap();
        s.lookup("x").cloned().unwrap()
    };
    assert_eq!(assigned("x=2"), Value::Int(2));
    assert_eq!(assigned("x=(1 + 2)"), Value::Int(3));
    assert_eq!(assigned("x=$y"), Value::String("v".into()));
    assert_eq!(assigned("x=$y/*.txt"), Value::String("v/*.txt".into()));
    assert_eq!(assigned("x=2abc"), Value::String("2abc".into()));
    assert_eq!(assigned("x='a b'"), Value::String("a b".into()));
    assert!(matches!(assigned("x=[a 1]"), Value::List(l) if l.len() == 2));
}

#[test]
fn expands_home_folders_at_the_start_of_bare_words() {
    assert_eq!(
        ok("echo ~ ~/a ~bob ~bob/x ~nobody/x '~' \"~/x\" a~ ~a~"),
        "/home/ada /home/ada/a /Users/bob /Users/bob/x ~nobody/x ~ ~/x a~ ~a~\n"
    );
    assert_eq!(ok("print [~/a ~bob]"), "/home/ada/a\n/Users/bob\n");
    assert_eq!(ok("cd /tmp\ncd ~\npwd"), "/home/ada\n");
    // The value of a NAME=value word does not start a word.
    let mut s = shell();
    run(&mut s, "TOOL=~/bin echo hi\nlet x = 1\nx=~/a").unwrap();
    let env: BTreeMap<_, _> = s.system.runs[0].programs[0]
        .environment
        .iter()
        .cloned()
        .collect();
    assert_eq!(env["TOOL"], "~/bin");
    assert_eq!(s.lookup("x"), Some(&Value::String("~/a".into())));
    let f = error("let s = 'x'\ndelete env.HOME\necho ~");
    assert_eq!(f.message, "`$env.HOME` is not set");
}

#[test]
fn redirects_built_in_commands_and_functions() {
    let mut s = shell();
    let out = run(
        &mut s,
        "print hi > /tmp/a\nprint more >> /tmp/a\nprint x >&2",
    )
    .unwrap();
    assert_eq!(out, "");
    assert_eq!(s.system.files["/tmp/a"], "hi\nmore\n");
    assert_eq!(String::from_utf8_lossy(&s.system.errors), "x\n");
    // A function's output takes in that of the programs it starts, and
    // a value that is given rather than written is written as text.
    run(
        &mut s,
        "fn f() { print a; echo b }\nf > /tmp/f\n[c d] | first 2 > /tmp/v",
    )
    .unwrap();
    assert_eq!(s.system.files["/tmp/f"], "a\nb\n");
    assert_eq!(s.system.files["/tmp/v"], "c\nd\n");
    s.system.files.insert("/tmp/in".into(), "x\ny\n".into());
    assert_eq!(run(&mut s, "lines < /tmp/in | len | print").unwrap(), "2\n");
    // 2> empties the file once, and the programs started append to it.
    s.system.files.insert("/tmp/err".into(), "old".into());
    s.system.runs.clear();
    run(
        &mut s,
        "fn g() { echo a; echo b 2> /tmp/own }\ng 2> /tmp/err",
    )
    .unwrap();
    assert_eq!(s.system.files["/tmp/err"], "");
    let taken = |n: usize| s.system.runs[n].programs[0].redirections.clone();
    assert_eq!(
        taken(0),
        [(RedirectionKind::ErrorAppend, "/tmp/err".into())]
    );
    assert_eq!(
        taken(1),
        [
            (RedirectionKind::ErrorAppend, "/tmp/err".into()),
            (RedirectionKind::Error, "/tmp/own".into())
        ]
    );
    s.system.runs.clear();
    run(&mut s, "echo after").unwrap();
    assert!(s.system.runs[0].programs[0].redirections.is_empty());
    assert_eq!(
        error("A=1 print x").message,
        "only programs take NAME=value words"
    );
}

#[test]
fn runs_programs_with_pipes_and_captures_output() {
    assert_eq!(ok("echo hello world | shout"), "HELLO WORLD\n");
    assert_eq!(ok("let x = (echo hi | shout)\nprint $x"), "HI\n");
    // A value going to a program is written as text.
    assert_eq!(ok("([a b]) | shout"), "A\nB\n");
    let f = error("nope");
    assert_eq!((f.status, f.message.as_str()), (127, "command not found"));
    let f = error("run false | cat");
    assert_eq!((f.status, f.command.as_str()), (1, "false"));
    assert_eq!(ok("print (\"ab\" | shout)"), "AB\n");
    // What a built-in command writes goes down the pipe.
    assert_eq!(ok("print x | shout"), "X\n");
    assert_eq!(ok("print x | upper | print"), "X\n");
    let mut s = shell();
    run(&mut s, "echo a 'b c' | shout").unwrap();
    assert_eq!(s.system.runs[0].text, "echo a b c | shout");
}

#[test]
fn sets_the_status_and_catches_errors() {
    assert_eq!(
        ok("try { run false } catch e { print $e.status $e.command $e.line }\nprint $status"),
        "1 false 1\n0\n"
    );
    assert_eq!(ok("echo x | cat\nprint $status"), "x\n0\n");
    assert_eq!(
        ok("try { print $x } catch e { print $e.message }"),
        "`$x` is not set\n"
    );
}

#[test]
fn branches_and_loops() {
    assert_eq!(ok("if 1 < 2 { print yes } else { print no }"), "yes\n");
    assert_eq!(ok("if false { print a } else if true { print b }"), "b\n");
    // A failing command is a false condition, not an error.
    assert_eq!(ok("if run false { print a } else { print b }"), "b\n");
    assert_eq!(
        ok("let i = 0\nwhile $i < 3 { i = $i + 1 }\nprint $i"),
        "3\n"
    );
    assert!(
        error("let i = 0\ni += 1")
            .message
            .contains("list or a string")
    );
    assert_eq!(ok("for x in [1 2] { print $x }"), "1\n2\n");
    assert_eq!(ok("for f in *.txt { print $f }"), "a.txt\nb.txt\n");
    assert!(error("if 3 { }").message.contains("bool"));
}

#[test]
fn matches_values() {
    let source = "for v in [0 5 [1 2 3] {name: 'x'} 'z'] {\n match $v {\n 0 => { print zero }\n 1..9 => { print small }\n [a ...rest] => { print $a $rest }\n {name: n} => { print $n }\n s if $s == 'z' => { print zed }\n }\n}";
    assert_eq!(ok(source), "zero\nsmall\n1 2\n3\nx\nzed\n");
    assert!(error("match 3 { 4 => { } }").message.contains("no arm"));
}

#[test]
fn calls_functions_with_arguments_flags_and_returns() {
    let defs = "fn greet(name, --loud = false, --by = 'msh') {\n if $loud { return \"HI {$name} from {$by}\" }\n (\"hi {$name} from {$by}\")\n}\n";
    assert_eq!(ok(&format!("{defs}print (greet ada)")), "hi ada from msh\n");
    assert_eq!(
        ok(&format!("{defs}print (greet ada --loud --by=sh)")),
        "HI ada from sh\n"
    );
    assert_eq!(
        ok(&format!("{defs}print (greet ada --by me)")),
        "hi ada from me\n"
    );
    assert!(
        error(&format!("{defs}greet"))
            .message
            .contains("takes 1 arguments")
    );
    assert!(
        error(&format!("{defs}greet a --x"))
            .message
            .contains("no flag")
    );
    // Program output in a function is its output; functions see globals,
    // not their caller's locals.
    assert_eq!(ok("fn f() { echo a | shout }\nprint (f)"), "A\n");
    assert_eq!(ok("let g = 1\nfn f() { ($g + 1) }\nprint (f)"), "2\n");
    assert!(
        error("fn f() { $local }\nfn h() { let local = 1\n f }\nh")
            .message
            .contains("not set")
    );
    // `(command)?` returns the error to the caller.
    assert_eq!(
        ok("fn f() { (/bin/false)?\n print never }\nprint (f).status"),
        "1\n"
    );
}

#[test]
fn shows_values_only_at_the_prompt() {
    assert_eq!(ok("(1 + 2)"), "");
    // A statement that starts like an expression is one; `run` runs a
    // command whose name is a value.
    assert_eq!(ok("1 + 2"), "");
    assert_eq!(ok("let p = 'echo'\nrun $p hi"), "hi\n");
    assert_eq!(error("let p = 'nope'\nrun $p").status, 127);
    let mut s = shell();
    s.interactive = true;
    assert_eq!(
        run(&mut s, "(1 + 2)\n([a b])\n(null)\n({x: 1})").unwrap(),
        "3\na\nb\nx\n1\n"
    );
}

#[test]
fn changes_folders_and_sources_scripts() {
    let mut s = shell();
    assert_eq!(run(&mut s, "cd /tmp\npwd").unwrap(), "/tmp\n");
    assert_eq!(s.system.cwd, "/tmp");
    run(&mut s, "cd -").unwrap();
    assert_eq!(s.system.cwd, "/home/ada");
    assert_eq!(scalar_text(s.env("OLDPWD").unwrap()), "/tmp");
    run(&mut s, "cd /tmp\ncd").unwrap();
    assert_eq!(s.system.cwd, "/home/ada");
    assert!(run(&mut s, "cd /nowhere").is_err());
    s.system.files.insert(
        "lib.msh".into(),
        "let shared = 5\nfn twice(x) { ($x * 2) }".into(),
    );
    assert_eq!(
        run(&mut s, "source lib.msh\nprint (twice $shared)").unwrap(),
        "10\n"
    );
    assert_eq!(run(&mut s, "exit 3"), Err(Flow::Exit(3)));
}

#[test]
fn controls_jobs_and_lists_history() {
    let mut s = shell();
    assert_eq!(
        run(&mut s, "jobs").unwrap(),
        format!("[1]+  {:<24}sleep 9\n", "Stopped")
    );
    run(&mut s, "bg %1").unwrap();
    // A job that ends with 3 in the foreground fails as it.
    assert!(matches!(run(&mut s, "fg"), Err(Flow::Error(f)) if f.status == 3));
    assert_eq!(
        s.system.resumed,
        [("%1".into(), false), ("%+".into(), true)]
    );
    run(
        &mut s,
        "kill %1\nkill -9 12\nkill -s HUP %2\nkill -SIGINT 4",
    )
    .unwrap();
    assert_eq!(
        s.system.killed,
        [
            ("%1".into(), 15),
            ("12".into(), 9),
            ("%2".into(), 1),
            ("4".into(), 2)
        ]
    );
    assert!(run(&mut s, "kill -FOO 1").is_err());
    let failed = run(&mut s, "exec echo hi");
    assert!(matches!(failed, Err(Flow::Error(f)) if f.message.contains("cannot replace")));
    s.history = vec!["ls".into(), "cd /".into()];
    assert_eq!(run(&mut s, "history").unwrap(), "    1  ls\n    2  cd /\n");
    s.history[1].folder = Some("/Users/ada".into());
    assert_eq!(
        run(&mut s, "history --folders").unwrap(),
        "    1  -  ls\n    2  /Users/ada  cd /\n"
    );
}

#[test]
fn gives_completion_its_functions_variables_programs_and_options() {
    use crate::edit::complete::complete;
    let mut s = shell();
    s.system.folders.insert(
        "/bin".into(),
        vec![
            ("echo".into(), false),
            ("shout".into(), false),
            ("lib".into(), true),
        ],
    );
    s.system
        .files
        .insert("job.msh".into(), "param --size: size = 1kb\nprint x".into());
    run(
        &mut s,
        "fn shine(x, --bright = 1) { $x }\nlet shade = 'dark'",
    )
    .unwrap();
    let texts = |s: &Shell<Fake>, line: &str| -> Vec<String> {
        let c = complete(line, line.len(), s);
        c.candidates.into_iter().map(|c| c.text).collect()
    };
    assert_eq!(texts(&s, "sh"), ["shine", "shout"]);
    assert_eq!(texts(&s, "shine a --"), ["--bright"]);
    assert_eq!(texts(&s, "print $sh"), ["$shade"]);
    assert_eq!(texts(&s, "print $env.HO"), ["$env.HOME"]);
    let c = complete("job.msh --", 10, &s);
    assert_eq!(c.candidates[0].text, "--size");
    assert_eq!(c.candidates[0].description.as_deref(), Some("size = 1kb"));
    assert_eq!(texts(&s, "cat b"), ["b.txt"]);
}
