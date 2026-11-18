//! sleep on Mikros: it waits for the sum of its durations, and refuses a
//! duration it cannot read before waiting at all.

use crate::duration::{COMMAND, total};
use mikros_cli::{INVALID, SUCCESS, complain, start};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    match total(&run.operands) {
        Ok(duration) => {
            std::thread::sleep(duration);
            ExitCode::from(SUCCESS)
        }
        Err(why) => {
            complain(&COMMAND, &run, &why);
            ExitCode::from(INVALID)
        }
    }
}
