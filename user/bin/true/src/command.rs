//! What true takes (MIKSPEC-0098.R54): any arguments, which it ignores,
//! except that `--help` alone shows its usage and msh's completion mode
//! is answered (MIKSPEC-0098.R31).

use mikros_cli::{Kind, Operand, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any("argument", Kind::Text, "ignored")],
    ..Spec::new("true", "Does nothing and succeeds, whatever its arguments.")
};

/// The environment variable that msh sets for completion mode.
const COMPLETE: &str = "MSH_COMPLETE";

/// Whether a run with `arguments` and the completion variable
/// `complete` answers, with its usage or candidates, instead of exiting
/// at once: only `--help` alone, and completion mode.
pub fn answers(arguments: &[String], complete: Option<&str>) -> bool {
    complete == Some("1") || arguments == ["--help"]
}

/// The value of msh's completion variable.
pub fn completion() -> Option<String> {
    std::env::var(COMPLETE).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(arguments: &[&str]) -> Vec<String> {
        arguments.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn answers_only_help_alone_and_completion() {
        assert!(answers(&words(&["--help"]), None));
        assert!(answers(&words(&["-x"]), Some("1")));
        assert!(!answers(&words(&[]), None));
        assert!(!answers(&words(&["--help", "x"]), None));
        assert!(!answers(&words(&["-x", "--nope", "y"]), Some("0")));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/true.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
