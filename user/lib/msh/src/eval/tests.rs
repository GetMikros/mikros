use super::*;
use crate::parse::parse;

/// A system without programs, whose current folder holds three files.
struct Fake;

impl glob::Folders for Fake {
    fn entries(&self, path: &str) -> Option<Vec<(String, bool)>> {
        (path == ".").then(|| {
            ["a.txt", "b.txt", "c.md"]
                .iter()
                .map(|n| (n.to_string(), false))
                .collect()
        })
    }
}

impl System for Fake {
    fn current_dir(&self) -> String {
        "/".into()
    }
    fn set_current_dir(&mut self, _: &str) -> Result<(), String> {
        Err("no folders".into())
    }
    fn read_file(&self, path: &str) -> Result<String, String> {
        Err(format!("{path}: not found"))
    }
    fn executable(&self, _: &str) -> Option<bool> {
        None
    }
    fn run(&mut self, _: Run) -> Result<Ran, String> {
        Err("no programs".into())
    }
    fn write(&mut self, _: Stream, _: &[u8]) {}
    fn jobs(&mut self) -> Vec<String> {
        Vec::new()
    }
    fn resume(&mut self, _: &str, _: bool) -> Result<Ran, String> {
        Err("no jobs".into())
    }
    fn kill(&mut self, _: &str, _: i32) -> Result<(), String> {
        Err("no processes".into())
    }
    fn exec(&mut self, _: Program) -> String {
        "no programs".into()
    }
}

fn shell() -> Shell<Fake> {
    Shell::new(Fake, [("PATH".into(), "/bin:/usr/bin".into())])
}

/// Runs `source` and gives the global variable `name`.
fn global(source: &str, name: &str) -> Value {
    let mut s = shell();
    let block = parse(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
    s.top(&block)
        .unwrap_or_else(|e| panic!("{source:?}: {e:?}"));
    s.lookup(name)
        .cloned()
        .unwrap_or_else(|| panic!("{name} is not set"))
}

fn failure(source: &str) -> Failure {
    let mut s = shell();
    match s.top(&parse(source).unwrap()) {
        Err(Flow::Error(f)) => f,
        other => panic!("{source:?}: {other:?}"),
    }
}

fn int(n: i64) -> Value {
    Value::Int(n)
}

#[test]
fn sets_and_changes_variables() {
    assert_eq!(global("let n = 2\nn = $n * 3", "n"), int(6));
    assert_eq!(
        global("let l = [1]\nl += 2", "l"),
        Value::List(vec![int(1), int(2)])
    );
    assert_eq!(
        global("let s = 'a'\ns += 'b'", "s"),
        Value::String("ab".into())
    );
    assert_eq!(global("let r = {a: [7 8]}\nlet x = $r.a.1", "x"), int(8));
    assert_eq!(global("let x = ($nope ?? 3)", "x"), int(3));
    assert_eq!(failure("let x = $nope").message, "`$nope` is not set");
    assert!(failure("nope = 1").message.contains("`let` creates it"));
    assert!(
        failure("let i = 0\ni += 1")
            .message
            .contains("list or a string")
    );
}

#[test]
fn reads_and_sets_the_environment() {
    assert_eq!(
        global("let p = $env.PATH", "p"),
        Value::List(vec![
            Value::String("/bin".into()),
            Value::String("/usr/bin".into())
        ])
    );
    let mut s = shell();
    s.top(&parse("env.PATH += '/opt/bin'\nenv.EDITOR = 'vi'").unwrap())
        .unwrap();
    let env: std::collections::BTreeMap<_, _> = s.program_environment(&[]).into_iter().collect();
    assert_eq!(env["PATH"], "/bin:/usr/bin:/opt/bin");
    assert_eq!(env["EDITOR"], "vi");
    s.top(&parse("env.EDITOR = null").unwrap()).unwrap();
    assert!(s.env("EDITOR").is_none());
}

#[test]
fn branches_loops_and_globs() {
    assert_eq!(
        global("let r = 0\nif 1 < 2 { r = 1 } else { r = 2 }", "r"),
        int(1)
    );
    assert_eq!(
        global("let r = 0\nif false { r = 1 } else if true { r = 2 }", "r"),
        int(2)
    );
    assert_eq!(
        global("let i = 0\nwhile $i < 3 { i = $i + 1 }", "i"),
        int(3)
    );
    assert_eq!(
        global("let t = 0\nfor x in [1 2 3] { t = $t + $x }", "t"),
        int(6)
    );
    assert_eq!(
        global("let l = []\nfor f in *.txt { l += $f }", "l"),
        Value::List(vec![
            Value::String("a.txt".into()),
            Value::String("b.txt".into())
        ])
    );
    assert!(failure("if 3 { }").message.contains("bool"));
    // Variables made in a block stay in it.
    assert!(
        failure("if true { let inner = 1 }\nlet x = $inner")
            .message
            .contains("not set")
    );
}

#[test]
fn matches_values_and_catches_errors() {
    let source = "let out = []\nfor v in [0 5 [1 2 3] {name: 'x'} 'z'] {\n match $v {\n 0 => { out += 'zero' }\n 1..9 => { out += 'small' }\n [a ...rest] => { out += $rest }\n {name: n} => { out += $n }\n s if $s == 'z' => { out += 'zed' }\n }\n}";
    let strings = |items: &[&str]| {
        items
            .iter()
            .map(|s| Value::String((*s).into()))
            .collect::<Vec<_>>()
    };
    let mut expected = strings(&["zero", "small"]);
    expected.push(Value::List(vec![int(2), int(3)]));
    expected.extend(strings(&["x", "zed"]));
    assert_eq!(global(source, "out"), Value::List(expected));
    assert!(failure("match 3 { 4 => { } }").message.contains("no arm"));
    assert_eq!(
        global(
            "let m = ''\ntry { let x = $nope } catch e { m = $e.message }",
            "m"
        ),
        Value::String("`$nope` is not set".into())
    );
}
