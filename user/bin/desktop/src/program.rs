//! desktop on Mikros: it calls `StartDesktop` on the session manager's
//! `Desktop`, which checks that the caller runs in a text console's
//! session.

use crate::{COMMAND, failure, nothing};
use mikros_cli::{FAILURE, SUCCESS, complain, start};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::session::DesktopClient;
use std::process::ExitCode;

const DESKTOP: &str = "mikros.session.Desktop";
const FOREVER: i64 = i64::MAX;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &nothing);
    let fail = |message: &str| {
        complain(&COMMAND, &run, message);
        ExitCode::from(FAILURE)
    };
    let Some(directory) = Directory::new(std::os::mikros::directory().as_raw_handle()) else {
        return fail("cannot reach the service directory");
    };
    let client = match directory.connect(DESKTOP) {
        Ok(client) => client,
        Err(why) => return fail(&why),
    };
    match DesktopClient::new(client).start_desktop(FOREVER) {
        Ok(Ok(_)) => ExitCode::from(SUCCESS),
        Ok(Err(status)) => fail(&failure(&format!("{status:?}"))),
        Err(error) => fail(&format!("StartDesktop failed: {error:?}")),
    }
}
