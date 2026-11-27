//! The grant lists of system apps (MIKSPEC-0037.R68): each base copy in
//! `/system/apps/<id>/` of the root file system may have a
//! `permissions.yaml`, which is registered with the broker, and the
//! registrations are ended once with `RegistrationsDone`.

use crate::supervisor::{Supervisor, now};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_manifest::{GRANTS_FILE, Source, names_role, parse_grant_list};
use mikros_rt::exec::Reactor;
use mikros_rt::files::{self, Failure};
use mikros_rt::interfaces::mikros::fs::{Error, NodeAsyncClient, NodeType};
use mikros_rt::interfaces::mikros::permissions::BrokerAsyncClient;

/// The folder of the base copies below the root's `/system`, and the
/// start of their IDs.
const APPS: &str = "apps";
const PREFIX: &str = "system.mikros.";
/// The manifest of an app, and the role of the app that logs users in.
const MANIFEST: &str = "manifest.yaml";
const GREETER: &str = "greeter";
/// How long after a failed call `RegistrationsDone` is called again.
const RETRY: i64 = 1_000_000_000;
/// How many calls of `RegistrationsDone` may fail before the service
/// manager goes on without them (MIKSPEC-0037.R68).
const CALLS: u32 = 6;

/// Registers the grant list of each base copy with the broker, then ends
/// the registrations. A list that cannot be read or has too many names is
/// logged and ignored, as are its entries that a list may not hold or
/// that the broker does not know; the others are registered.
pub async fn register(supervisor: &Supervisor, system: &NodeAsyncClient) {
    let Some(broker) = supervisor.broker.borrow().clone() else {
        mikros_rt::log!(
            Error,
            "the permission broker is not running; grant lists are not registered"
        );
        return;
    };
    register_lists(&supervisor.reactor, system, &broker).await;
    // A call that fails is made again a second later, until one succeeds
    // or `CALLS` have failed; the services outside the boot image start
    // after that (MIKSPEC-0037.R68).
    for failed in 1..=CALLS {
        if try_done(&broker).await {
            return;
        }
        if failed < CALLS {
            let _ = supervisor
                .reactor
                .sleep_until(now().saturating_add(RETRY))
                .await;
        }
    }
    mikros_rt::log!(
        Error,
        "the registrations were not ended after {CALLS} failed calls; going on without"
    );
}

/// Calls `RegistrationsDone` once and logs a failure.
async fn try_done(broker: &BrokerAsyncClient) -> bool {
    match broker.registrations_done().await {
        Ok(Ok(_)) => true,
        Ok(Err(status)) => {
            mikros_rt::log!(Error, "ending the registrations failed: {status:?}");
            false
        }
        Err(e) => {
            mikros_rt::log!(Error, "ending the registrations failed: {e:?}");
            false
        }
    }
}

async fn register_lists(reactor: &Reactor, system: &NodeAsyncClient, broker: &BrokerAsyncClient) {
    let ids = match files::list(system, reactor, APPS, NodeType::Directory, "").await {
        Ok(ids) => ids,
        Err(Failure::Fs(Error::NotFound)) => return,
        Err(failure) => {
            mikros_rt::log!(Error, "listing /system/{APPS} failed: {failure:?}");
            return;
        }
    };
    let mut known = None;
    for id in ids.into_iter().filter(|id| id.starts_with(PREFIX)) {
        let path = format!("/system/{APPS}/{id}/{GRANTS_FILE}");
        let relative = files::join(&files::join(APPS, &id), GRANTS_FILE);
        let bytes = match files::read(system, reactor, &relative, mikros_yaml::SIZE_MAX).await {
            Ok(bytes) => bytes,
            Err(Failure::Fs(Error::NotFound)) => continue,
            Err(failure) => {
                mikros_rt::log!(Error, "reading {path} failed: {failure:?}");
                continue;
            }
        };
        let manifest = files::join(&files::join(APPS, &id), MANIFEST);
        let greeter = match files::read(system, reactor, &manifest, mikros_yaml::SIZE_MAX).await {
            Ok(manifest) => names_role(&manifest, GREETER),
            Err(_) => false,
        };
        let list = match parse_grant_list(&bytes, Source::List { greeter }) {
            Ok(list) => list,
            Err(e) => {
                let kind = e.kind;
                mikros_rt::log!(Error, "{path}:{}:{}: {kind:?}; ignored", e.line, e.column);
                continue;
            }
        };
        for (name, why) in &list.rejected {
            mikros_rt::log!(Error, "{path}: {name} is ignored: {why:?}");
        }
        if known.is_none() {
            known = Some(known_permissions(broker).await);
        }
        let known = known.as_deref().unwrap_or_default();
        let (names, unknown): (Vec<String>, Vec<String>) = list
            .names
            .into_iter()
            .partition(|name| known.binary_search(name).is_ok());
        for name in unknown {
            mikros_rt::log!(
                Error,
                "{path}: {name} is ignored: the broker does not know it"
            );
        }
        match broker.grant_list(id.clone(), names).await {
            Ok(Ok(_)) => {}
            Ok(Err(status)) => mikros_rt::log!(Error, "registering {id}: {status:?}"),
            Err(e) => mikros_rt::log!(Error, "registering {id}: {e:?}"),
        }
    }
}

/// The names of the permissions that the broker knows, sorted: the pages
/// of `ListPermissions` up to the empty one.
pub async fn known_permissions(broker: &BrokerAsyncClient) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    loop {
        let after = names.last().cloned().unwrap_or_default();
        match broker.list_permissions(after).await {
            Ok(Ok(page)) if !page.permissions.is_empty() => names.extend(page.permissions),
            Ok(Ok(_)) => return names,
            failed => {
                mikros_rt::log!(Error, "listing the permissions failed: {failed:?}");
                return names;
            }
        }
    }
}
