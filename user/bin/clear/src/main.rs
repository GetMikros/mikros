//! clear, which clears the terminal's screen and its scrollback: it moves
//! the cursor to the top left, erases the screen and erases the lines
//! scrolled off it.

use mikros_cli::{Arguments, Command, FAILURE, INVALID, SUCCESS};
use std::io::Write;
use std::process::ExitCode;

/// The cursor to the top left, erasing the screen, and erasing the
/// scrollback.
const CLEAR: &[u8] = b"\x1b[H\x1b[2J\x1b[3J";

const COMMAND: Command = Command {
    name: "clear",
    operands: "",
    most: 0,
    about: "Clears the terminal's screen and its scrollback.",
    options: &[],
};

fn main() -> ExitCode {
    let text = match COMMAND.parse(std::env::args().skip(1)) {
        Arguments::Run { .. } => CLEAR,
        Arguments::Help => &COMMAND.help().into_bytes()[..],
        Arguments::Invalid(reason) => {
            eprintln!("{}", COMMAND.error(&reason));
            return ExitCode::from(INVALID);
        }
    };
    let mut out = std::io::stdout().lock();
    match out.write_all(text).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(error) => {
            eprintln!("{}", COMMAND.error(&format!("cannot write: {error}")));
            ExitCode::from(FAILURE)
        }
    }
}
