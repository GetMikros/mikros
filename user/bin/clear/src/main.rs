//! clear, which clears the terminal's screen and its scrollback: it moves
//! the cursor to the top left, erases the screen and erases the lines
//! scrolled off it.

use mikros_cli::{FAILURE, Kind, SUCCESS, Spec, complain, start};
use std::io::Write;
use std::process::ExitCode;

/// The cursor to the top left, erasing the screen, and erasing the
/// scrollback.
const CLEAR: &[u8] = b"\x1b[H\x1b[2J\x1b[3J";

static COMMAND: Spec = Spec::new("clear", "Clears the terminal's screen and its scrollback.");

/// Names of system things for completion, of which clear takes none.
fn nothing(_: Kind, _: &str) -> Vec<String> {
    Vec::new()
}

fn main() -> ExitCode {
    let run = start(&COMMAND, &nothing);
    let mut out = std::io::stdout().lock();
    match out.write_all(CLEAR).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(error) => {
            complain(&COMMAND, &run, &format!("cannot write: {error}"));
            ExitCode::from(FAILURE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    #[test]
    fn takes_no_operands() {
        assert!(matches!(COMMAND.parse(Vec::new()), Parsed::Run(_)));
        assert_eq!(
            COMMAND.parse(vec!["x".to_string()]),
            Parsed::Invalid("unexpected argument x".into())
        );
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/clear.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
