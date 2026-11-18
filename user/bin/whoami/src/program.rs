//! whoami on Mikros: the C library's `getuid` gives the caller's user
//! ID, also under sudo (MIKSPEC-0092.R9), and the accounts service's
//! `ListUsers` its name.

use crate::user::{COMMAND, SYSTEM, name_in, shown};
use mikros_cli::{FAILURE, SUCCESS, complain, start};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::session::AccountsClient;
use std::io::Write;
use std::process::ExitCode;

const ACCOUNTS: &str = "mikros.session.Accounts";
const FOREVER: i64 = i64::MAX;

unsafe extern "C" {
    fn getuid() -> u32;
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    // SAFETY: the call takes no arguments.
    let id = unsafe { getuid() };
    let name = match id {
        0 => Ok(SYSTEM.to_string()),
        id => name_of(id),
    };
    let name = match name {
        Ok(name) => name,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(FAILURE);
        }
    };
    let text = shown(&name, id, run.has("--json"));
    match std::io::stdout().write_all(text.as_bytes()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(_) => ExitCode::from(FAILURE),
    }
}

/// The name of the user `id`, from the page of `ListUsers` that starts
/// at it.
fn name_of(id: u32) -> Result<String, String> {
    let directory = Directory::new(std::os::mikros::directory().as_raw_handle())
        .ok_or("cannot reach the service directory")?;
    let accounts = AccountsClient::new(directory.connect(ACCOUNTS)?);
    let users = accounts
        .list_users(id - 1, FOREVER)
        .map_err(|e| format!("ListUsers failed: {e:?}"))?
        .users;
    let users: Vec<(u32, String)> = users.into_iter().map(|u| (u.id, u.name)).collect();
    name_in(id, &users)
        .map(str::to_string)
        .ok_or_else(|| format!("cannot find the name of user ID {id}"))
}
