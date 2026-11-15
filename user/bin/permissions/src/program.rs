//! permissions on Mikros: it finds a named user through the accounts
//! service's `ListUsers`, asks
//! the session manager's `Authorization` with its own identity when the
//! request needs it, and then calls the permission broker.

use crate::listing::{self, COMMAND, Decision, Grant, Request};
use mikros_cli::{FAILURE, INVALID, SUCCESS};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::permissions::{
    BrokerClient, Decision as WireDecision, Grant as WireGrant,
};
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use mikros_rt::interfaces::mikros::session::AccountsClient;
use std::io::Write;
use std::process::ExitCode;

const BROKER: &str = "mikros.permissions.Broker";
const ACCOUNTS: &str = "mikros.session.Accounts";
const APP_ID: &str = "system.permissions";
const ACTION: &str = "permissions.manage";
const FOREVER: i64 = i64::MAX;

unsafe extern "C" {
    fn getuid() -> u32;
}

pub fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let request = match listing::read(arguments.clone()) {
        Request::Help => return write(&COMMAND.help()),
        Request::Invalid(reason) => return fail(INVALID, &reason),
        request => request,
    };
    // SAFETY: the call takes no pointers.
    let caller = unsafe { getuid() };
    let named = match &request {
        Request::List { user, .. } | Request::Set { user, .. } => user.as_deref(),
        Request::Help | Request::Invalid(_) => None,
    };
    let Some(directory) = Directory::new(std::os::mikros::directory().as_raw_handle()) else {
        return fail(FAILURE, "cannot reach the service directory");
    };
    let user = match named {
        None => caller,
        Some(name) => match uid(&directory, name) {
            Ok(Some(uid)) => uid,
            Ok(None) => return fail(FAILURE, &format!("{name}: no such user")),
            Err(why) => return fail(FAILURE, &why),
        },
    };
    if listing::needs_authorization(&request, user == caller)
        && !authorized(&directory, caller, &arguments)
    {
        return fail(FAILURE, "not authorized");
    }
    let broker = match directory.connect(BROKER) {
        Ok(client) => BrokerClient::new(client),
        Err(why) => return fail(FAILURE, &why),
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
                    false => write(&listing::text(&grants)),
                }
            }
            Ok(Err(status)) => fail(FAILURE, &failure(None, &format!("{status:?}"))),
            Err(error) => fail(FAILURE, &format!("ListGrants failed: {error:?}")),
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
                Ok(Err(status)) => {
                    fail(FAILURE, &failure(Some(&permission), &format!("{status:?}")))
                }
                Err(error) => fail(FAILURE, &format!("SetGrant failed: {error:?}")),
            }
        }
        Request::Help | Request::Invalid(_) => unreachable!("answered above"),
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

/// Writes the error `message` to standard error and gives `status`.
fn fail(status: u8, message: &str) -> ExitCode {
    eprintln!("{}", COMMAND.error(message));
    ExitCode::from(status)
}

/// Whether an administrator allows `permissions.manage` for this
/// process of user `caller`, asked with the command line as the reason.
fn authorized(directory: &Directory, caller: u32, arguments: &[String]) -> bool {
    let me = ClientIdentity {
        pid: std::process::id(),
        user: caller,
        app_id: APP_ID.into(),
        admin: 0,
        koid: 0,
    };
    let reason = format!("permissions {}", arguments.join(" "));
    directory.authorized(me, ACTION, reason.trim_end().into())
}
