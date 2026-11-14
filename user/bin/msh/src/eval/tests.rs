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
    fn read_file(&self, path: &str) -> Result<String, String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("{path}: not found"))
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
