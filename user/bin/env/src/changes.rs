//! What env takes, and the environment it gives the command or writes
//! (MIKSPEC-0098.R54). Its options end at its first operand, since the
//! operands are settings and then a command to run (MIKSPEC-0098.R31).

use mikros_cli::{Kind, Operand, Opt, Run, Spec};
use std::ffi::OsString;

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "command",
        Kind::Command,
        "name=value settings, then the command to run and its arguments",
    )],
    options: &[
        Opt::both("ignore-environment", 'i', "start with an empty environment"),
        Opt::both("unset", 'u', "remove the variable from the environment")
            .takes("name", Kind::Text),
        Opt::both("chdir", 'C', "run the command in this folder").takes("folder", Kind::Folder),
        Opt::both(
            "null",
            '0',
            "end each variable written with NUL, not a line feed",
        ),
    ],
    stops: true,
    ..Spec::new(
        "env",
        "Runs the command with the environment changed by the options and the\n\
         name=value settings, or writes the environment so changed without one.",
    )
};

/// What a run asks for.
#[derive(Debug, PartialEq)]
pub struct Plan {
    pub clear: bool,
    pub unset: Vec<String>,
    pub set: Vec<(String, String)>,
    pub folder: Option<String>,
    pub null: bool,
    /// The command and its arguments, empty to write the environment.
    pub command: Vec<String>,
}

/// What `run` asks for, or why its arguments are invalid.
pub fn plan(run: &Run) -> Result<Plan, String> {
    let unset: Vec<String> = run.values("-u").into_iter().map(String::from).collect();
    if let Some(bad) = unset.iter().find(|n| n.is_empty() || n.contains('=')) {
        return Err(format!("cannot unset {bad:?}: not a variable name"));
    }
    let mut operands = run.operands.iter();
    let mut set = Vec::new();
    let mut command = Vec::new();
    for operand in operands.by_ref() {
        match operand.split_once('=') {
            Some(("", _)) => return Err(format!("cannot set {operand:?}: no variable name")),
            Some((name, value)) => set.push((name.to_string(), value.to_string())),
            None => {
                command.push(operand.clone());
                break;
            }
        }
    }
    command.extend(operands.cloned());
    let plan = Plan {
        clear: run.has("-i"),
        unset,
        set,
        folder: run.value("-C").map(String::from),
        null: run.has("-0"),
        command,
    };
    if plan.null && !plan.command.is_empty() {
        return Err("-0 cannot be used with a command".into());
    }
    if plan.folder.is_some() && plan.command.is_empty() {
        return Err("-C needs a command".into());
    }
    Ok(plan)
}

/// The environment that `plan` makes of `current`: emptied with `-i`,
/// without the variables of `-u`, and with the settings, each in the
/// place of the variable it replaces or else at the end.
pub fn apply(current: Vec<(OsString, OsString)>, plan: &Plan) -> Vec<(OsString, OsString)> {
    let mut vars = match plan.clear {
        true => Vec::new(),
        false => current,
    };
    vars.retain(|(name, _)| !plan.unset.iter().any(|u| name == u.as_str()));
    for (name, value) in &plan.set {
        match vars.iter_mut().find(|(n, _)| n == name.as_str()) {
            Some((_, v)) => *v = value.into(),
            None => vars.push((name.into(), value.into())),
        }
    }
    vars
}

/// `vars` as env writes them: `name=value`, each ended by a line feed,
/// or by NUL with `null`.
pub fn written(vars: &[(OsString, OsString)], null: bool) -> Vec<u8> {
    let end = if null { b'\0' } else { b'\n' };
    let mut out = Vec::new();
    for (name, value) in vars {
        out.extend_from_slice(name.as_encoded_bytes());
        out.push(b'=');
        out.extend_from_slice(value.as_encoded_bytes());
        out.push(end);
    }
    out
}

/// The status env exits with for a command that exited with `code`, or
/// was ended by `signal`: the command's own, or 128 and the signal, as
/// shells give it.
pub fn status(code: Option<i32>, signal: Option<i32>) -> u8 {
    match (code, signal) {
        (Some(code), _) => (code & 0xff) as u8,
        (None, Some(signal)) => (128 + (signal & 0x7f)) as u8,
        (None, None) => mikros_cli::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn run(arguments: &[&str]) -> Run {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => run,
            other => panic!("{other:?}"),
        }
    }

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs.iter().map(|(n, v)| (n.into(), v.into())).collect()
    }

    #[test]
    fn reads_settings_then_the_command() {
        let plan = plan(&run(&["-i", "-u", "A", "B=1", "C=", "ls", "-l", "D=2"])).unwrap();
        assert!(plan.clear && !plan.null);
        assert_eq!(plan.unset, ["A"]);
        assert_eq!(
            plan.set,
            [("B".into(), "1".into()), ("C".into(), String::new())]
        );
        assert_eq!(plan.command, ["ls", "-l", "D=2"]);
        // Options end at the first operand.
        let plan = super::plan(&run(&["X=1", "-i"])).unwrap();
        assert!(!plan.clear);
        assert_eq!(plan.command, ["-i"]);
    }

    #[test]
    fn refuses_what_it_cannot_do() {
        let refused = |arguments: &[&str]| super::plan(&run(arguments)).unwrap_err();
        assert_eq!(refused(&["=1"]), "cannot set \"=1\": no variable name");
        assert_eq!(
            refused(&["-u", "A=B"]),
            "cannot unset \"A=B\": not a variable name"
        );
        assert_eq!(refused(&["-0", "true"]), "-0 cannot be used with a command");
        assert_eq!(refused(&["-C", "/tmp"]), "-C needs a command");
    }

    #[test]
    fn changes_the_environment() {
        let current = vars(&[("A", "1"), ("B", "2"), ("C", "3")]);
        let plan = super::plan(&run(&["-u", "A", "B=x", "D=4"])).unwrap();
        assert_eq!(
            apply(current.clone(), &plan),
            vars(&[("B", "x"), ("C", "3"), ("D", "4")])
        );
        let plan = super::plan(&run(&["-i", "D=4"])).unwrap();
        assert_eq!(apply(current, &plan), vars(&[("D", "4")]));
    }

    #[test]
    fn writes_the_environment() {
        let vars = vars(&[("A", "1"), ("B", "x=y")]);
        assert_eq!(written(&vars, false), b"A=1\nB=x=y\n");
        assert_eq!(written(&vars, true), b"A=1\0B=x=y\0");
    }

    #[test]
    fn exits_with_the_command_status() {
        assert_eq!(status(Some(3), None), 3);
        assert_eq!(status(None, Some(15)), 143);
        assert_eq!(status(None, None), 1);
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/env.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
