//! The part of the greeter that runs on Mikros: the screen, drawn with
//! Elegance (MIKSPEC-0066.R24), and the connection to the `Login` service.

mod login;
mod screen;

use std::process::ExitCode;

pub fn run() -> ExitCode {
    screen::run()
}
