//! The test suites of the ported projects, which the `port-tests` runner
//! runs on Mikros from the boot image `ports.img`: the tests Meson lists
//! for a port, checked against what `ports/<name>/tests.txt` expects of
//! them, the plan the runner follows, the files the tests need, and the
//! tests that run on the build host instead.

use crate::ports::{self, Built, Port};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Where the runner reads its plan, in the boot image.
pub const PLAN: &str = "usr/share/port-tests/plan";

/// How many times the timeout Meson gives a test it has on Mikros, where
/// it may run emulated.
const SLOWER: u64 = 10;

/// What a test is expected to do.
#[derive(Debug, PartialEq)]
enum Expected {
    /// It passes on Mikros.
    Pass,
    /// It runs on Mikros and fails, until what it needs is there.
    Fail,
    /// It passes on the build host.
    Host,
    /// It is not run.
    Excluded,
}

/// Whether `word` is a requirement ID, such as `MIKSPEC-0033.R8`.
fn requirement_id(word: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    word.strip_prefix("MIKSPEC-")
        .and_then(|w| w.split_once(".R"))
        .is_some_and(|(doc, n)| doc.len() == 4 && digits(doc) && digits(n))
}

/// The expectations of `text`, a `tests.txt`: a line for each test, with
/// its name and `pass` or `host`, or `expected-fail` or `excluded` and
/// the IDs of the requirements that say why. Blank lines and those that
/// begin with `#` are skipped.
fn expectations(text: &str) -> Result<BTreeMap<String, Expected>> {
    let mut expected = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let (name, what) = (words.next(), words.next());
        let why: Vec<&str> = words.map(|w| w.trim_end_matches(',')).collect();
        let ids = !why.is_empty() && why.iter().all(|w| requirement_id(w));
        let what = match (what, why.is_empty()) {
            (Some("pass"), true) => Expected::Pass,
            (Some("host"), true) => Expected::Host,
            (Some("expected-fail"), _) if ids => Expected::Fail,
            (Some("excluded"), _) if ids => Expected::Excluded,
            _ => bail!(
                "line {}: not a test and `pass`, `host`, or `expected-fail` or \
                 `excluded` and requirement IDs",
                i + 1
            ),
        };
        let name = name.unwrap_or_default();
        if expected.insert(name.to_owned(), what).is_some() {
            bail!("line {}: {name} is listed twice", i + 1);
        }
    }
    Ok(expected)
}

/// A test as the runner runs it.
#[derive(Debug, PartialEq)]
struct Test {
    name: String,
    path: String,
    args: Vec<String>,
    env: Vec<String>,
    timeout: u64,
    expect_fail: bool,
}

/// The plan that has the runner run `tests`.
fn plan(tests: &[Test]) -> String {
    let mut plan = String::new();
    for test in tests {
        plan += &format!("test {}\npath {}\n", test.name, test.path);
        for arg in &test.args {
            plan += &format!("arg {arg}\n");
        }
        for var in &test.env {
            plan += &format!("env {var}\n");
        }
        if test.expect_fail {
            plan += "expect fail\n";
        }
        plan += &format!("timeout {}\nend\n", test.timeout);
    }
    plan
}

/// The host directories of a port's build and source, and their paths
/// on Mikros.
struct Paths {
    build: String,
    source: String,
    programs: String,
    test_source: String,
}

impl Paths {
    /// `value` with a leading host directory replaced by its path on
    /// Mikros.
    fn on_mikros(&self, value: &str) -> String {
        if let Some(rest) = value.strip_prefix(&self.build) {
            format!("{}{rest}", self.programs)
        } else if let Some(rest) = value.strip_prefix(&self.source) {
            format!("{}{rest}", self.test_source)
        } else {
            value.to_owned()
        }
    }
}

/// The tests of `port` that run on Mikros, which `listed`, Meson's list
/// of the tests of its build, holds, with their paths on Mikros. Fails
/// unless `expected` names each test of `listed` and no other.
fn tests(
    port: &Port,
    paths: &Paths,
    listed: &Value,
    expected: &BTreeMap<String, Expected>,
) -> Result<Vec<Test>> {
    let listed = listed.as_array().context("the test list is not an array")?;
    let mut tests = Vec::new();
    let mut seen = Vec::new();
    for test in listed {
        let field = |key: &str| {
            test.get(key)
                .with_context(|| format!("a test has no {key}"))
        };
        let name = field("name")?.as_str().context("a test's name")?;
        seen.push(name);
        let expect_fail = match expected.get(name) {
            Some(Expected::Pass) => false,
            Some(Expected::Fail) => true,
            Some(Expected::Host | Expected::Excluded) => continue,
            None => bail!("ports/{}/tests.txt does not list {name}", port.name),
        };
        if field("protocol")?.as_str() != Some("exitcode") {
            bail!("{name} does not report its result by its exit code");
        }
        if !field("workdir")?.is_null() {
            bail!("{name} runs in a directory of its own");
        }
        let strings = |key: &str| -> Result<Vec<String>> {
            let values = field(key)?.as_array().context(key.to_owned())?;
            values
                .iter()
                .map(|v| v.as_str().map(|s| paths.on_mikros(s)))
                .collect::<Option<_>>()
                .with_context(|| format!("{name}'s {key}"))
        };
        let args = strings("cmd")?;
        let path = args
            .first()
            .with_context(|| format!("{name} has no command"))?;
        if !path.starts_with(&paths.programs) {
            bail!("{name} runs a program from outside the build directory");
        }
        let env = field("env")?.as_object().context("a test's environment")?;
        let env = env
            .iter()
            // The libraries are in `/usr/lib`, where the dynamic linker
            // finds them.
            .filter(|(var, _)| *var != "LD_LIBRARY_PATH")
            .map(|(var, value)| Some(format!("{var}={}", paths.on_mikros(value.as_str()?))))
            .collect::<Option<_>>()
            .with_context(|| format!("{name}'s environment"))?;
        let timeout = field("timeout")?.as_u64().context("a test's timeout")?;
        tests.push(Test {
            name: format!("{}/{name}", port.name),
            path: path.clone(),
            args,
            env,
            timeout: timeout * SLOWER,
            expect_fail,
        });
    }
    if let Some(name) = expected.keys().find(|n| !seen.contains(&n.as_str())) {
        bail!(
            "ports/{}/tests.txt lists {name}, which the suite lacks",
            port.name
        );
    }
    Ok(tests)
}

/// Meson's list of the tests of `port`, built in `built`, and what
/// `ports/<name>/tests.txt` expects of them.
fn suite(port: &Port, built: &Built) -> Result<(Value, BTreeMap<String, Expected>)> {
    let listed = crate::port::output(
        crate::meson::command()?
            .args(["introspect", "--tests"])
            .arg(&built.build),
    )
    .with_context(|| format!("listing the tests of {}", port.name))?;
    let listed: Value = serde_json::from_str(&listed).context("reading Meson's test list")?;
    let file = crate::workspace_root()
        .join("ports")
        .join(port.name)
        .join("tests.txt");
    let text = fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let expected = expectations(&text).with_context(|| format!("in {}", file.display()))?;
    Ok((listed, expected))
}

/// Runs the tests of `port`, built in `built`, that `tests.txt` has run
/// on the build host, each as Meson would: its command, in the build
/// directory, with its environment added. Fails unless each passes.
pub fn run_on_host(port: &Port, built: &Built) -> Result<()> {
    let (listed, expected) = suite(port, built)?;
    let listed = listed.as_array().context("the test list is not an array")?;
    for test in listed {
        let name = test["name"].as_str().context("a test's name")?;
        if expected.get(name) != Some(&Expected::Host) {
            continue;
        }
        let cmd: Option<Vec<&str>> = test["cmd"]
            .as_array()
            .and_then(|c| c.iter().map(Value::as_str).collect());
        let cmd = cmd.with_context(|| format!("{name}'s command"))?;
        let (program, args) = cmd.split_first().context("a test has no command")?;
        let env = test["env"].as_object().context("a test's environment")?;
        let mut command = std::process::Command::new(program);
        command.args(args).current_dir(&built.build);
        for (var, value) in env {
            command.env(var, value.as_str().context("a test's environment")?);
        }
        crate::port::run(&mut command).with_context(|| format!("{}/{name}", port.name))?;
        println!("passed   {}/{name} on the build host", port.name);
    }
    Ok(())
}

/// The members of the boot image of the tests of `port`, built in
/// `built`, by their names there, besides the runner and the dynamic
/// linker: the plan, written to `plan_file`, the programs the tests run,
/// the shared libraries of the build directory by their major versions,
/// and the files of `port`'s test data.
pub fn members(port: &Port, built: &Built, plan_file: &Path) -> Result<Vec<(String, PathBuf)>> {
    let (listed, expected) = suite(port, built)?;
    let host = |dir: &Path| {
        dir.to_str()
            .map(str::to_owned)
            .context("a path is not UTF-8")
    };
    let paths = Paths {
        build: host(&built.build)?,
        source: host(&built.source)?,
        programs: ports::test_programs(port),
        test_source: ports::test_source(port),
    };
    let tests = tests(port, &paths, &listed, &expected)?;
    fs::write(plan_file, plan(&tests))?;
    let mut members = vec![(PLAN.to_owned(), plan_file.to_owned())];
    for test in &tests {
        let relative = &test.path[paths.programs.len() + 1..];
        members.push((test.path[1..].to_owned(), built.build.join(relative)));
    }
    for entry in fs::read_dir(&built.build)? {
        let name = entry?.file_name().into_string().ok().unwrap_or_default();
        if major_version_name(&name) {
            members.push((format!("usr/lib/{name}"), built.build.join(&name)));
        }
    }
    for dir in port.test_data {
        let mut files = Vec::new();
        collect(&built.source.join(dir), &mut files)?;
        for path in files {
            let relative = path
                .strip_prefix(&built.source)?
                .to_str()
                .context("not UTF-8")?;
            members.push((format!("{}/{relative}", &paths.test_source[1..]), path));
        }
    }
    Ok(members)
}

/// Whether `name` is a library's name with its major version only:
/// `lib<name>.<major>.dlib`.
fn major_version_name(name: &str) -> bool {
    let Some(stem) = name
        .strip_prefix("lib")
        .and_then(|n| n.strip_suffix(".dlib"))
    else {
        return false;
    };
    let number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    // A full version has more numbers before the last.
    stem.rsplit_once('.').is_some_and(|(name, major)| {
        number(major) && !name.rsplit_once('.').is_some_and(|(_, n)| number(n))
    })
}

/// Adds every regular file below `dir` to `files`, in name order.
fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_expectations() {
        let text = "# comment\n\na pass\nb excluded MIKSPEC-0033.R8\n\
                    c expected-fail MIKSPEC-0050.R9, MIKSPEC-0033.R17\nd host\n";
        let expected = expectations(text).unwrap();
        assert_eq!(expected["a"], Expected::Pass);
        assert_eq!(expected["b"], Expected::Excluded);
        assert_eq!(expected["c"], Expected::Fail);
        assert_eq!(expected["d"], Expected::Host);
        assert!(expectations("a excluded\n").is_err());
        assert!(expectations("a excluded needs fork\n").is_err());
        assert!(expectations("a expected-fail MIKSPEC-33.R8\n").is_err());
        assert!(expectations("a pass MIKSPEC-0033.R8\n").is_err());
        assert!(expectations("a pass\na pass\n").is_err());
        assert!(expectations("a maybe\n").is_err());
    }

    fn paths() -> Paths {
        Paths {
            build: "/h/build".into(),
            source: "/h/src".into(),
            programs: "/usr/libexec/p-tests".into(),
            test_source: "/usr/share/p-tests".into(),
        }
    }

    #[test]
    fn plans_the_tests_run_on_mikros_with_paths_there() {
        let listed: Value = serde_json::from_str(
            r#"[
                {"name": "a", "cmd": ["/h/build/test-a", "x"], "protocol": "exitcode",
                 "workdir": null, "timeout": 30,
                 "env": {"top_srcdir": "/h/src", "LD_LIBRARY_PATH": "/h/build/"}},
                {"name": "b", "cmd": ["/h/build/test-b"], "protocol": "exitcode",
                 "workdir": null, "timeout": 30, "env": {}},
                {"name": "c", "cmd": ["/h/build/test-c"], "protocol": "exitcode",
                 "workdir": null, "timeout": 5, "env": {}},
                {"name": "d", "cmd": ["/h/src/check.py"], "protocol": "exitcode",
                 "workdir": null, "timeout": 30, "env": {}}
            ]"#,
        )
        .unwrap();
        let text = "a pass\nb excluded MIKSPEC-0033.R8\nc expected-fail MIKSPEC-0050.R9\nd host\n";
        let expected = expectations(text).unwrap();
        let tests = tests(&ports::XKBCOMMON, &paths(), &listed, &expected).unwrap();
        assert_eq!(
            tests,
            [
                Test {
                    name: "xkbcommon/a".into(),
                    path: "/usr/libexec/p-tests/test-a".into(),
                    args: vec!["/usr/libexec/p-tests/test-a".into(), "x".into()],
                    env: vec!["top_srcdir=/usr/share/p-tests".into()],
                    timeout: 300,
                    expect_fail: false,
                },
                Test {
                    name: "xkbcommon/c".into(),
                    path: "/usr/libexec/p-tests/test-c".into(),
                    args: vec!["/usr/libexec/p-tests/test-c".into()],
                    env: vec![],
                    timeout: 50,
                    expect_fail: true,
                },
            ]
        );
        assert_eq!(
            plan(&tests[..1]),
            "test xkbcommon/a\npath /usr/libexec/p-tests/test-a\n\
             arg /usr/libexec/p-tests/test-a\narg x\n\
             env top_srcdir=/usr/share/p-tests\ntimeout 300\nend\n"
        );
        assert!(plan(&tests[1..]).ends_with("expect fail\ntimeout 50\nend\n"));
        let missing = expectations("a pass\n").unwrap();
        assert!(super::tests(&ports::XKBCOMMON, &paths(), &listed, &missing).is_err());
        let extra = expectations(&format!("{text}e pass\n")).unwrap();
        assert!(super::tests(&ports::XKBCOMMON, &paths(), &listed, &extra).is_err());
    }

    #[test]
    fn names_libraries_by_their_major_versions() {
        assert!(major_version_name("libxkbcommon.0.dlib"));
        assert!(major_version_name("libxkbcommon-test-internal.0.dlib"));
        assert!(!major_version_name("libxkbcommon.0.13.2.dlib"));
        assert!(!major_version_name("libxkbcommon.dlib"));
        assert!(!major_version_name("test-keysym"));
    }
}
