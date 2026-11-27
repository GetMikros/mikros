//! Reading service, program and driver manifests, and choosing the order
//! in which the boot services start.
//!
//! `parse` reads one service manifest file into a `Service`,
//! `parse_program` one program manifest file into a `Program` and
//! `parse_driver` one driver manifest into a `Driver`, or says why the
//! file is invalid and where. `order` takes the services of the
//! valid files and returns the boot services in an order that starts
//! each one after those it waits for, with the services that cannot
//! start and why.

#![no_std]

extern crate alloc;

mod driver;
mod grants;
mod order;
pub mod path;
pub mod profiles;
mod program;
#[cfg(test)]
mod tests;

pub use driver::{
    Choice, DRIVER_PREFIX, Driver, DriverKind, DriverManifest, Need, choose, installed_may_match,
    is_hardware_id, parse_driver,
};
pub use grants::{
    GRANTS_FILE, GrantList, MAX_GRANTS, Rejected, Source, check_grant, parse_grant_list,
};
pub use order::{Blocked, Order, order};
pub use program::{NamespaceEntry, Program, ProgramManifest, parse_program};

use alloc::string::String;
use alloc::vec::Vec;
use mikros_yaml::{Map, Node};

/// The keys of a manifest.
const KEYS: &[&str] = &[
    "name",
    "binary",
    "provides",
    "after",
    "start",
    "restart",
    "boot_handles",
    "permissions",
    "namespace",
    "priority",
    "stop_timeout",
];
/// The keys of an item of `provides`.
const PROVIDES_KEYS: &[&str] = &["protocol", "permission"];
/// Those of a package's service, which may also say who may connect.
const PACKAGE_PROVIDES_KEYS: &[&str] = &["protocol", "permission", "own_app", "allow"];
/// `priority` when the manifest leaves it out.
pub const PRIORITY_DEFAULT: i64 = 16;
/// `stop_timeout`, in seconds, when the manifest leaves it out.
pub const STOP_TIMEOUT_DEFAULT: u64 = 10;
const SUFFIX: &str = ".yaml";
/// The prefix of the app IDs of system services, which name their
/// folders in `/system/services/`.
pub const SERVICE_PREFIX: &str = "system.";
/// The file name of a service's manifest in its folder.
pub const SERVICE_FILE: &str = "service.yaml";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    /// The app ID it runs with: `system.<name>` for a service of the
    /// system, and its package's ID for one of a package.
    pub app_id: String,
    pub binary: String,
    pub provides: Vec<Provide>,
    pub after: Vec<String>,
    pub start: Start,
    pub restart: Restart,
    pub boot_handles: Vec<BootHandle>,
    pub permissions: Vec<String>,
    pub namespace: Vec<NamespaceEntry>,
    pub priority: i64,
    /// How long the service has to end after `TERM`, in seconds.
    pub stop_timeout: u64,
}

/// A protocol that a service serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provide {
    pub protocol: String,
    pub permission: Option<String>,
    /// For a package's service, whether only its own app may connect
    /// (MIKSPEC-0101.R66).
    pub own_app: bool,
    /// For a package's service, the other app IDs that may connect; any
    /// app may when it is empty and `own_app` is false.
    pub allow: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Boot,
    OnDemand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restart {
    Always,
    OnFailure,
    Never,
}

/// A handle that init gives the service manager, which passes it on
/// only to the services whose manifest lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootHandle {
    RootResource,
    LogManage,
    RootBudget,
    Framebuffer,
    PlatformInfo,
}

/// A valid manifest: its service, and the keys it holds that a
/// manifest does not have, for logging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub service: Service,
    pub unknown: Vec<Unknown>,
}

/// A key that the reader ignored, and where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unknown {
    pub key: String,
    pub line: usize,
    pub column: usize,
}

/// Why a manifest is invalid, and where: the line and column, from 1,
/// of the first error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub column: usize,
    pub kind: ErrorKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The file is not a valid YAML file.
    Yaml(mikros_yaml::ErrorKind),
    /// A key that a manifest must have is absent.
    Missing(&'static str),
    /// `name` is not lowercase kebab-case.
    BadName,
    /// A service's or driver's folder is not named by the app ID of
    /// `name`, or a program's manifest file not after its `app_id`.
    FileName,
    /// `binary` is not an absolute path under `/system`.
    BadBinary,
    /// `start` is neither `boot` nor `on-demand`.
    BadStart,
    /// `restart` is not `always`, `on-failure` or `never`.
    BadRestart,
    /// `boot_handles` holds a value that names no boot handle.
    BadBootHandle,
    /// A program's `app_id` does not start with `system.`, or is one that
    /// no program manifest may name.
    BadAppId,
    /// A driver manifest has this key of service manifests.
    ServiceKey(&'static str),
    /// A driver's `kind` is neither `normal` nor `fallback`.
    BadKind,
    /// `needs` holds a value other than `dma`, `ioports` and `bus`.
    BadNeed,
    /// `match` holds a string that is not a hardware ID.
    BadHardwareId,
    /// An installed driver's `needs` holds `bus` or `ioports`.
    InstalledNeed,
    /// An installed driver's `match` holds an ID other than the vendor
    /// and device IDs of a PCI function or a USB interface.
    InstalledMatch,
    /// `permissions` lists a permission that no manifest of its kind may
    /// list.
    ReservedPermission,
    /// The manifest of a command has this key, which only other programs'
    /// manifests may have.
    CommandKey(&'static str),
    /// A grant list holds more names than a list may.
    TooManyPermissions,
    /// A package's service provides a protocol whose name does not start
    /// with the package's ID and `.`.
    BadProtocol,
}

impl From<mikros_yaml::Error> for Error {
    fn from(e: mikros_yaml::Error) -> Self {
        Error {
            line: e.line,
            column: e.column,
            kind: ErrorKind::Yaml(e.kind),
        }
    }
}

fn error(node: &Node, kind: ErrorKind) -> Error {
    Error {
        line: node.line,
        column: node.column,
        kind,
    }
}

/// Reads the manifest in `bytes`, from the file `service.yaml` in the
/// folder `folder`, which must be `system.<name>`.
pub fn parse(folder: &str, bytes: &[u8]) -> Result<Manifest, Error> {
    let root = mikros_yaml::parse(bytes)?;
    let map = root.map()?;
    let name_node = map
        .get("name")
        .ok_or(error(&root, ErrorKind::Missing("name")))?;
    let name = name_node.string()?;
    if !is_kebab_case(name) {
        return Err(error(name_node, ErrorKind::BadName));
    }
    if folder.strip_prefix(SERVICE_PREFIX) != Some(name) {
        return Err(error(name_node, ErrorKind::FileName));
    }
    parse_service(
        &root,
        String::from(name),
        String::from(folder),
        is_under_system,
        false,
    )
}

/// The service manifest `bytes` of the service `key` of the package `id`
/// (MIKSPEC-0101.R55): its `name` is `key`, it runs as `<id>.<key>` with
/// the app ID `id` and a binary in `/apps/<id>/`, and it has no boot
/// handles and no `system` or `admin` permission.
pub fn parse_package_service(id: &str, key: &str, bytes: &[u8]) -> Result<Manifest, Error> {
    let root = mikros_yaml::parse(bytes)?;
    let map = root.map()?;
    let name_node = map
        .get("name")
        .ok_or(error(&root, ErrorKind::Missing("name")))?;
    let name = name_node.string()?;
    if !is_kebab_case(name) {
        return Err(error(name_node, ErrorKind::BadName));
    }
    if name != key {
        return Err(error(name_node, ErrorKind::FileName));
    }
    let package = alloc::format!("/apps/{id}/");
    let in_package = |binary: &str| {
        binary
            .strip_prefix(package.as_str())
            .is_some_and(|b| !b.is_empty())
    };
    let manifest = parse_service(
        &root,
        alloc::format!("{id}.{key}"),
        String::from(id),
        in_package,
        true,
    )?;
    // Its protocols are named under the package's ID.
    let own = alloc::format!("{id}.");
    if let Some(at) = manifest
        .service
        .provides
        .iter()
        .position(|p| !p.protocol.starts_with(own.as_str()))
    {
        let node = list(map, "provides")?.get(at).unwrap_or(&root);
        return Err(error(node, ErrorKind::BadProtocol));
    }
    if let Some(node) = map.get("boot_handles") {
        return Err(error(node, ErrorKind::BadBootHandle));
    }
    let reserved = |p: &String| p.starts_with("system.") || p.starts_with("admin.");
    if manifest.service.permissions.iter().any(reserved) {
        let node = map.get("permissions").unwrap_or(&root);
        return Err(error(node, ErrorKind::ReservedPermission));
    }
    Ok(manifest)
}

/// The service manifest of `root` for the service `name` with the app ID
/// `app_id`, whose binary `binary_ok` accepts.
fn parse_service(
    root: &Node,
    name: String,
    app_id: String,
    binary_ok: impl Fn(&str) -> bool,
    package: bool,
) -> Result<Manifest, Error> {
    let map = root.map()?;
    let mut unknown = unknown_keys(map, KEYS);
    let required = |key| map.get(key).ok_or(error(root, ErrorKind::Missing(key)));
    let binary_node = required("binary")?;
    let binary = binary_node.string()?;
    if !binary_ok(binary) {
        return Err(error(binary_node, ErrorKind::BadBinary));
    }
    let start_node = required("start")?;
    let start = match start_node.string()? {
        "boot" => Start::Boot,
        "on-demand" => Start::OnDemand,
        _ => return Err(error(start_node, ErrorKind::BadStart)),
    };
    let restart = match map.get("restart") {
        None => Restart::Never,
        Some(node) => match node.string()? {
            "always" => Restart::Always,
            "on-failure" => Restart::OnFailure,
            "never" => Restart::Never,
            _ => return Err(error(node, ErrorKind::BadRestart)),
        },
    };
    let mut provides = Vec::new();
    for item in list(map, "provides")? {
        let entry = item.map()?;
        let keys = if package {
            PACKAGE_PROVIDES_KEYS
        } else {
            PROVIDES_KEYS
        };
        unknown.extend(unknown_keys(entry, keys));
        let protocol = entry
            .get("protocol")
            .ok_or(error(item, ErrorKind::Missing("protocol")))?;
        let (own_app, allow) = match package {
            true => (
                entry
                    .get("own_app")
                    .map(Node::boolean)
                    .transpose()?
                    .unwrap_or(false),
                strings(entry, "allow")?,
            ),
            false => (false, Vec::new()),
        };
        provides.push(Provide {
            protocol: protocol.string()?.into(),
            permission: entry
                .get("permission")
                .map(Node::string)
                .transpose()?
                .map(Into::into),
            own_app,
            allow,
        });
    }
    let mut boot_handles = Vec::new();
    for item in list(map, "boot_handles")? {
        boot_handles.push(match item.string()? {
            "root_resource" => BootHandle::RootResource,
            "log_manage" => BootHandle::LogManage,
            "root_budget" => BootHandle::RootBudget,
            "framebuffer" => BootHandle::Framebuffer,
            "platform_info" => BootHandle::PlatformInfo,
            _ => return Err(error(item, ErrorKind::BadBootHandle)),
        });
    }
    let priority = match map.get("priority") {
        None => PRIORITY_DEFAULT,
        Some(node) => node.integer(i64::MIN, i64::MAX)?,
    };
    // A positive integer.
    let stop_timeout = match map.get("stop_timeout") {
        None => STOP_TIMEOUT_DEFAULT,
        Some(node) => node.integer(1, i64::MAX)? as u64,
    };
    let service = Service {
        name,
        app_id,
        binary: binary.into(),
        provides,
        after: strings(map, "after")?,
        start,
        restart,
        boot_handles,
        permissions: permissions(map)?,
        namespace: program::namespace(map, &mut unknown)?,
        priority,
        stop_timeout,
    };
    Ok(Manifest { service, unknown })
}

/// The items of the list at `key`, or none when it is absent.
fn list<'a>(map: Map<'a>, key: &str) -> Result<&'a [Node], mikros_yaml::Error> {
    map.get(key).map_or(Ok(&[]), Node::list)
}

/// The permissions that no service or program manifest may list: the
/// service manager uses `system.log_sources.create` without a manifest.
const RESERVED_PERMISSIONS: [&str; 1] = ["system.log_sources.create"];

/// The permissions at `permissions`, or none when it is absent; a
/// service or program manifest that lists one of `RESERVED_PERMISSIONS`
/// is invalid.
fn permissions(map: Map<'_>) -> Result<Vec<String>, Error> {
    let mut permissions = Vec::new();
    for item in list(map, "permissions")? {
        let permission = item.string()?;
        if RESERVED_PERMISSIONS.contains(&permission) {
            return Err(error(item, ErrorKind::ReservedPermission));
        }
        permissions.push(permission.into());
    }
    Ok(permissions)
}

/// The list of strings at `key`, or none when it is absent.
fn strings(map: Map<'_>, key: &str) -> Result<Vec<String>, mikros_yaml::Error> {
    list(map, key)?
        .iter()
        .map(|item| item.string().map(Into::into))
        .collect()
}

fn unknown_keys(map: Map<'_>, known: &[&str]) -> Vec<Unknown> {
    map.unknown(known)
        .map(|e| Unknown {
            key: e.key.clone(),
            line: e.line,
            column: e.column,
        })
        .collect()
}

/// Whether `name` matches `[a-z][a-z0-9]*(-[a-z0-9]+)*`.
fn is_kebab_case(name: &str) -> bool {
    let mut parts = name.split('-');
    let first = parts.next().unwrap_or("");
    let alnum = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    };
    first.starts_with(|c: char| c.is_ascii_lowercase()) && alnum(first) && parts.all(alnum)
}

/// Whether `path` is absolute and names something below `/system`.
/// Empty, `.` and `..` components are refused, so that the path cannot
/// leave `/system`.
fn is_under_system(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/system/") else {
        return false;
    };
    rest.split('/').all(|c| !matches!(c, "" | "." | ".."))
}
