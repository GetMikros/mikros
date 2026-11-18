//! What pwd takes, and which path it writes (MIKSPEC-0098.R41): with
//! `-L`, which it takes when neither is given, `$PWD` when that is an
//! absolute path without `.` or `..` that names the current folder, and
//! otherwise, as with `-P`, the current folder with its links resolved.

use mikros_cli::{Opt, Run, Spec};
use std::path::{Component, Path};

pub static COMMAND: Spec = Spec {
    options: &[
        Opt::both(
            "logical",
            'L',
            "write $PWD when it names the current folder (the default)",
        ),
        Opt::both(
            "physical",
            'P',
            "write the current folder with its links resolved",
        ),
    ],
    ..Spec::new(
        "pwd",
        "Writes the path of the current folder. The last of -L and -P wins.",
    )
};

/// Whether `run` asks for the folder with its links resolved: whether
/// the last of `-L` and `-P` is `-P`, by either form.
pub fn physical(run: &Run) -> bool {
    run.last_of(&["-L", "-P"]) == Some("-P")
}

/// Whether `pwd` may be written for `-L` if it names the current folder:
/// an absolute path without `.` or `..` components.
pub fn plausible(pwd: &str) -> bool {
    let path = Path::new(pwd);
    path.is_absolute()
        && !pwd.split('/').any(|part| part == "." || part == "..")
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(arguments: &[&str]) -> Vec<String> {
        arguments.iter().map(|a| a.to_string()).collect()
    }

    fn run(arguments: &[&str]) -> mikros_cli::Run {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            mikros_cli::Parsed::Run(run) => run,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_last_of_l_and_p_wins() {
        assert!(!physical(&run(&[])));
        assert!(physical(&run(&["-P"])));
        assert!(!physical(&run(&["-PL"])));
        assert!(physical(&run(&["-L", "--physical"])));
        assert!(!physical(&run(&[
            "--physical",
            "--color=never",
            "--logical"
        ])));
    }

    #[test]
    fn takes_only_absolute_pwds_without_dots() {
        assert!(plausible("/users/ada/src"));
        assert!(plausible("/"));
        assert!(plausible("/a//b/"));
        assert!(!plausible("users/ada"));
        assert!(!plausible("/a/./b"));
        assert!(!plausible("/a/.."));
        assert!(!plausible(""));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/pwd.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
