//! The names of system things that msh's completion offers
//! (MIKSPEC-0084.R68, MIKSPEC-0098.R31): services, users, installed apps,
//! which are the packages of kind `app` (MIKSPEC-0101.R4), the
//! permissions that the broker knows (MIKSPEC-0037.R49), volumes and
//! devices, each from the service that keeps them, page by
//! page. A thing whose service cannot be reached has no names.

use crate::Directory;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::interfaces::mikros::driver::{DevicesClient, VolumesClient};
use mikros_rt::interfaces::mikros::packages::InstallerClient;
use mikros_rt::interfaces::mikros::permissions::{AppNumbersClient, BrokerClient};
use mikros_rt::interfaces::mikros::service::ServicesClient;
use mikros_rt::interfaces::mikros::session::AccountsClient;

const SERVICES: &str = "mikros.service.Services";
const ACCOUNTS: &str = "mikros.session.Accounts";
const INSTALLER: &str = "mikros.packages.Installer";
const BROKER: &str = "mikros.permissions.Broker";
const APP_NUMBERS: &str = "mikros.permissions.AppNumbers";
const VOLUMES: &str = "device.volumes";
const DEVICES: &str = "mikros.driver.Devices";
/// How long each page may take, so that completion stays within msh's
/// second.
const PAGE_TIME: i64 = 250_000_000;

/// A kind of system thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Thing {
    Service,
    User,
    App,
    /// The permissions that the broker knows.
    Permission,
    Volume,
    /// As `<class>/<name>`.
    Device,
}

impl Directory {
    /// The app ID of the app number `number`, which access control
    /// lists hold, when the broker knows it.
    pub fn app_id(&self, number: u32) -> Option<String> {
        let client = AppNumbersClient::new(self.connect(APP_NUMBERS).ok()?);
        let deadline = now().saturating_add(PAGE_TIME);
        client.app_id(number, deadline).ok()?.ok().map(|r| r.app_id)
    }

    /// The names of every thing of kind `thing`.
    pub fn names(&self, thing: Thing) -> Vec<String> {
        let deadline = now().saturating_add(4 * PAGE_TIME);
        match thing {
            Thing::Service => self.services(deadline),
            Thing::User => self.users(deadline),
            Thing::App => self.apps(deadline),
            Thing::Permission => self.permissions(deadline),
            Thing::Volume => self.volumes(deadline),
            Thing::Device => self.devices(deadline),
        }
    }

    fn services(&self, deadline: i64) -> Vec<String> {
        let Ok(client) = self.connect(SERVICES) else {
            return Vec::new();
        };
        let client = ServicesClient::new(client);
        let mut names: Vec<String> = Vec::new();
        loop {
            let Ok(page) = client.list(names.last().cloned(), deadline) else {
                return names;
            };
            names.extend(page.entries.into_iter().map(|e| e.name));
            if !page.more {
                return names;
            }
        }
    }

    fn users(&self, deadline: i64) -> Vec<String> {
        let Ok(client) = self.connect(ACCOUNTS) else {
            return Vec::new();
        };
        let client = AccountsClient::new(client);
        let (mut names, mut after) = (Vec::new(), 0);
        while let Ok(page) = client.list_users(after, deadline) {
            let Some(last) = page.users.last().map(|u| u.id) else {
                break;
            };
            names.extend(page.users.into_iter().map(|u| u.name));
            after = last;
        }
        names
    }

    fn apps(&self, deadline: i64) -> Vec<String> {
        let Ok(client) = self.connect(INSTALLER) else {
            return Vec::new();
        };
        let client = InstallerClient::new(client);
        let mut names: Vec<String> = Vec::new();
        let mut after = String::new();
        while let Ok(Ok(page)) = client.list(after.clone(), deadline) {
            let Some(last) = page.packages.last().map(|p| p.id.clone()) else {
                break;
            };
            // Libraries and drivers are packages too, but not apps.
            names.extend(
                page.packages
                    .into_iter()
                    .filter(|p| p.kind == "app")
                    .map(|p| p.id),
            );
            after = last;
        }
        names
    }

    fn permissions(&self, deadline: i64) -> Vec<String> {
        let Ok(client) = self.connect(BROKER) else {
            return Vec::new();
        };
        let client = BrokerClient::new(client);
        let mut names: Vec<String> = Vec::new();
        loop {
            let after = names.last().cloned().unwrap_or_default();
            match client.list_permissions(after, deadline) {
                Ok(Ok(page)) if !page.permissions.is_empty() => names.extend(page.permissions),
                _ => return names,
            }
        }
    }

    fn volumes(&self, deadline: i64) -> Vec<String> {
        let Ok(client) = self.connect(VOLUMES) else {
            return Vec::new();
        };
        let client = VolumesClient::new(client);
        let mut names: Vec<String> = Vec::new();
        let mut after = String::new();
        while let Ok(Ok(page)) = client.list(after.clone(), deadline) {
            let Some(last) = page.volumes.last().map(|v| v.id.clone()) else {
                break;
            };
            names.extend(page.volumes.into_iter().filter(|v| !v.hidden).map(|v| v.id));
            after = last;
        }
        names
    }

    fn devices(&self, deadline: i64) -> Vec<String> {
        let Ok(client) = self.connect(DEVICES) else {
            return Vec::new();
        };
        let client = DevicesClient::new(client);
        let mut names: Vec<String> = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let Ok(page) = client.list(None, after.clone(), deadline) else {
                return names;
            };
            after = page
                .entries
                .last()
                .map(|e| format!("{}/{}", e.class, e.name));
            names.extend(
                page.entries
                    .into_iter()
                    .map(|e| format!("{}/{}", e.class, e.name)),
            );
            if !page.more || after.is_none() {
                return names;
            }
        }
    }
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { mikros_rt::sys::clock_get_monotonic(&mut now) };
    now
}
