//! What selftest makes of its arguments and of how each test ended.

use std::collections::BTreeSet;

/// What the arguments ask for.
#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    /// Whether to power the machine off once the tests ran.
    pub poweroff: bool,
    /// The tests named, or none for every test.
    pub names: Vec<String>,
}

/// The options of `args`, the arguments after the program's name.
pub fn options(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        poweroff: false,
        names: Vec::new(),
    };
    for arg in args {
        match arg.as_str() {
            "--poweroff" => options.poweroff = true,
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            name => options.names.push(name.to_owned()),
        }
    }
    Ok(options)
}

/// The tests to run, in the order of their names: every test of
/// `available`, or those of `named`, which must all be available.
pub fn selected(available: &[String], named: &[String]) -> Result<Vec<String>, String> {
    let available: BTreeSet<&String> = available.iter().collect();
    if named.is_empty() {
        return Ok(available.into_iter().cloned().collect());
    }
    let mut chosen = BTreeSet::new();
    for name in named {
        if !available.contains(name) {
            return Err(format!("there is no test {name}"));
        }
        chosen.insert(name.clone());
    }
    Ok(chosen.into_iter().collect())
}

/// How a test ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// It exited with this status.
    Exited(i32),
    /// A signal ended it.
    Signal(i32),
    /// It ran out of time and was killed.
    Timeout,
}

impl Ending {
    pub fn passed(self) -> bool {
        self == Ending::Exited(0)
    }
}

/// The line that says how the test `name` ended.
pub fn line(name: &str, ending: Ending) -> String {
    match ending {
        Ending::Exited(0) => format!("selftest: PASS {name}"),
        Ending::Exited(status) => format!("selftest: FAIL {name} {status}"),
        Ending::Signal(signal) => format!("selftest: FAIL {name} signal {signal}"),
        Ending::Timeout => format!("selftest: FAIL {name} timeout"),
    }
}

/// The last line, after every test ran.
pub fn summary(endings: &[Ending]) -> String {
    let passed = endings.iter().filter(|e| e.passed()).count();
    format!(
        "selftest: {passed} passed, {} failed",
        endings.len() - passed
    )
}

/// selftest's exit status after `endings`.
pub fn status(endings: &[Ending]) -> i32 {
    match endings.iter().all(|e| e.passed()) {
        true => 0,
        false => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reads_the_options() {
        let o = options(&strings(&["--poweroff", "b", "a"])).unwrap();
        assert!(o.poweroff);
        assert_eq!(o.names, strings(&["b", "a"]));
        assert!(options(&strings(&["--fast"])).is_err());
        assert_eq!(options(&[]).unwrap().names, Vec::<String>::new());
    }

    #[test]
    fn runs_the_tests_in_the_order_of_their_names() {
        let available = strings(&["files", "clock", "spawn"]);
        assert_eq!(
            selected(&available, &[]).unwrap(),
            strings(&["clock", "files", "spawn"])
        );
        assert_eq!(
            selected(&available, &strings(&["spawn", "clock"])).unwrap(),
            strings(&["clock", "spawn"])
        );
        assert!(selected(&available, &strings(&["nope"])).is_err());
    }

    #[test]
    fn words_each_ending() {
        assert_eq!(line("a", Ending::Exited(0)), "selftest: PASS a");
        assert_eq!(line("a", Ending::Exited(3)), "selftest: FAIL a 3");
        assert_eq!(line("a", Ending::Signal(9)), "selftest: FAIL a signal 9");
        assert_eq!(line("a", Ending::Timeout), "selftest: FAIL a timeout");
    }

    #[test]
    fn sums_up() {
        let endings = [Ending::Exited(0), Ending::Timeout, Ending::Exited(0)];
        assert_eq!(summary(&endings), "selftest: 2 passed, 1 failed");
        assert_eq!(status(&endings), 1);
        assert_eq!(status(&endings[..1]), 0);
        assert_eq!(status(&[]), 0);
    }
}
