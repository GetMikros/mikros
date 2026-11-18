//! less, which shows a file or its standard input one screen at a time
//! on the terminal of its standard output, or copies it when that is not
//! a terminal. `COMMAND` is what it takes, `keys` reads the terminal's
//! keys, `pager` keeps what is shown and moves it, and `program`, left
//! out of the host's build, runs them on the terminal.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

use mikros_cli::{Kind, Operand, Spec};

mod keys;
mod pager;
#[cfg(target_os = "mikros")]
mod program;

pub static COMMAND: Spec = Spec {
    operands: &[Operand::optional(
        "file",
        Kind::File,
        "the file to show, standard input when none is named",
    )],
    // less shows its help itself, not through another less.
    pages_help: false,
    ..Spec::new(
        "less",
        "Shows the file, or standard input when no file is named, one screen\n\
         at a time. When standard output is not a terminal, copies the input\n\
         to it. Space, f and Page Down show the next screen, b and Page Up\n\
         the previous one, Enter, j and Down the next line, k and Up the\n\
         previous one, g and Home the first line, G and End the last screen,\n\
         / followed by text and Enter the next line that contains the text,\n\
         n and N the next and the previous line that contains it again, and\n\
         q quits.",
    )
};

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    #[test]
    fn takes_at_most_one_file() {
        let parse = |a: &[&str]| COMMAND.parse(a.iter().map(|a| a.to_string()));
        assert!(matches!(parse(&["notes"]), Parsed::Run(r) if r.operands == ["notes"]));
        assert_eq!(
            parse(&["a", "b"]),
            Parsed::Invalid("unexpected argument b".into())
        );
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/less.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
