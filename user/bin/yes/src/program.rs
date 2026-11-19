//! yes on Mikros: it writes its line to standard output until writing
//! fails, which is its end when the reader closes the pipe
//! (MIKSPEC-0098.R50).

use crate::repeating::{self, COMMAND};
use mikros_cli::{FAILURE, SUCCESS, complain, start};
use std::io::{self, Write};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let block = repeating::block(&repeating::line(&run.operands), 8192);
    let mut out = io::stdout().lock();
    loop {
        if let Err(e) = out.write_all(&block) {
            return match e.kind() {
                io::ErrorKind::BrokenPipe => ExitCode::from(SUCCESS),
                _ => {
                    complain(&COMMAND, &run, &format!("standard output: {e}"));
                    ExitCode::from(FAILURE)
                }
            };
        }
    }
}
