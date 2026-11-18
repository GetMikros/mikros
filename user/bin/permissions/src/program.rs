//! permissions on Mikros: it finds a named user through the accounts
//! service's `ListUsers`, and then calls the permission broker, which
//! asks an administrator when the call needs one.

use crate::listing::{self, COMMAND, Decision, Grant, Request};
use mikros_cli::{FAILURE, Paint, Run, SUCCESS, complain, start};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::permissions::{
    BrokerClient, Decision as WireDecision, Grant as WireGrant,
};
use mikros_rt::interfaces::mikros::session::AccountsClient;
use std::io::Write;
use std::process::ExitCode;

const BROKER: &str = "mikros.permissions.Broker";
const ACCOUNTS: &str = "mikros.session.Accounts";
const FOREVER: i64 = i64::MAX;

unsafe extern "C" {
    fn getuid() -> u32;
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let fail = |message: &str| fail(&run, message);
    let request = listing::read(&run);
    // SAFETY: the call takes no pointers.
    let caller = unsafe { getuid() };
    let named = match &request {
        Request::List { user, .. } | Request::Set { user, .. } => user.as_deref(),
    };
    let Some(directory) = Directory::new(std::os::mikros::directory().as_raw_handle()) else {
        return fail("cannot reach the service directory");
    };
    let user = match named {
        None => caller,
        Some(name) => match uid(&directory, name) {
            Ok(Some(uid)) => uid,
            Ok(None) => return fail(&format!("{name}: no such user")),
            Err(why) => return fail(&why),
        },
    };
    let broker = match directory.connect(BROKER) {
        Ok(client) => BrokerClient::new(client),
        Err(why) => return fail(&why),
    };
    match request {
        Request::List { json, .. } => match broker.list_grants(user, FOREVER) {
            Ok(Ok(listed)) => {
                let grants: Vec<Grant> = listed
                    .grants
                    .into_iter()
                    .filter_map(|g| {
                        let decision = match g.decision {
                            WireDecision::Allowed => "allowed",
                            WireDecision::Denied => "denied",
                            _ => return None,
                        };
                        Some(Grant {
                            user: g.user,
                            app_id: g.app_id,
                            permission: g.permission,
                            decision,
                        })
                    })
                    .collect();
                match json {
                    true => write(&listing::json(&grants)),
                    false => write(&listing::text(&grants, Paint::stdout(&run))),
                }
            }
            Ok(Err(status)) => fail(&failure(None, &format!("{status:?}"))),
            Err(error) => fail(&format!("ListGrants failed: {error:?}")),
        },
        Request::Set {
            decision,
            app_id,
            permission,
            ..
        } => {
            let grant = WireGrant {
                user,
                app_id,
                permission: permission.clone(),
                decision: match decision {
                    Decision::Allow => WireDecision::Allowed,
                    Decision::Deny => WireDecision::Denied,
                },
            };
            match broker.set_grant(grant, FOREVER) {
                Ok(Ok(_)) => ExitCode::from(SUCCESS),
                Ok(Err(status)) => fail(&failure(Some(&permission), &format!("{status:?}"))),
                Err(error) => fail(&format!("SetGrant failed: {error:?}")),
            }
        }
    }
}

fn failure(permission: Option<&str>, status: &str) -> String {
    listing::failure(permission, &listing::lowercase(status))
}

/// The user ID of the user named `name`, from the accounts' list page
/// by page.
fn uid(directory: &Directory, name: &str) -> Result<Option<u32>, String> {
    let accounts = AccountsClient::new(directory.connect(ACCOUNTS)?);
    let mut after = 0;
    loop {
        let users = accounts
            .list_users(after, FOREVER)
            .map_err(|e| format!("ListUsers failed: {e:?}"))?
            .users;
        let Some(last) = users.last().map(|u| u.id) else {
            return Ok(None);
        };
        if let Some(user) = users.iter().find(|u| u.name == name) {
            return Ok(Some(user.id));
        }
        after = last;
    }
}

/// Writes `text` to standard output.
fn write(text: &str) -> ExitCode {
    match std::io::stdout().write_all(text.as_bytes()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(_) => ExitCode::from(FAILURE),
    }
}

/// Writes the error `message` to standard error and gives the status of
/// a failure.
fn fail(run: &Run, message: &str) -> ExitCode {
    complain(&COMMAND, run, message);
    ExitCode::from(FAILURE)
}
