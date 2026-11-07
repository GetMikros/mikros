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
mod order;
mod program;
#[cfg(test)]
mod tests;

pub use driver::{
    Choice, Driver, DriverKind, DriverManifest, Need, choose, is_hardware_id, parse_driver,
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
];
/// The keys of an item of `provides`.
const PROVIDES_KEYS: &[&str] = &["protocol", "permission"];
/// `priority` when the manifest leaves it out.
pub const PRIORITY_DEFAULT: i64 = 16;
const SUFFIX: &str = ".yaml";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub binary: String,
    pub provides: Vec<Provide>,
    pub after: Vec<String>,
    pub start: Start,
    pub restart: Restart,
    pub boot_handles: Vec<BootHandle>,
    pub permissions: Vec<String>,
    pub namespace: Vec<NamespaceEntry>,
    pub priority: i64,
}

/// A protocol that a service serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provide {
    pub protocol: String,
    pub permission: Option<String>,
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
    /// The file, or a driver manifest's directory, is not named after
    /// `name`.
    FileName,
    /// `binary` is not an absolute path under `/usr`.
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

/// Reads the manifest in `bytes`, from the file `file_name`.
pub fn parse(file_name: &str, bytes: &[u8]) -> Result<Manifest, Error> {
    let root = mikros_yaml::parse(bytes)?;
    let map = root.map()?;
    let mut unknown = unknown_keys(map, KEYS);
    let required = |key| map.get(key).ok_or(error(&root, ErrorKind::Missing(key)));

    let name_node = required("name")?;
    let name = name_node.string()?;
    if !is_kebab_case(name) {
        return Err(error(name_node, ErrorKind::BadName));
    }
    if file_name.strip_suffix(SUFFIX) != Some(name) {
        return Err(error(name_node, ErrorKind::FileName));
    }
    let binary_node = required("binary")?;
    let binary = binary_node.string()?;
    if !is_under_usr(binary) {
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
        unknown.extend(unknown_keys(entry, PROVIDES_KEYS));
        let protocol = entry
            .get("protocol")
            .ok_or(error(item, ErrorKind::Missing("protocol")))?;
        provides.push(Provide {
            protocol: protocol.string()?.into(),
            permission: entry
                .get("permission")
                .map(Node::string)
                .transpose()?
                .map(Into::into),
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
    let service = Service {
        name: name.into(),
        binary: binary.into(),
        provides,
        after: strings(map, "after")?,
        start,
        restart,
        boot_handles,
        permissions: strings(map, "permissions")?,
        namespace: program::namespace(map, &mut unknown)?,
        priority,
    };
    Ok(Manifest { service, unknown })
}

/// The items of the list at `key`, or none when it is absent.
fn list<'a>(map: Map<'a>, key: &str) -> Result<&'a [Node], mikros_yaml::Error> {
    map.get(key).map_or(Ok(&[]), Node::list)
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

/// Whether `path` is absolute and names something below `/usr`. Empty,
/// `.` and `..` components are refused, so that the path cannot leave
/// `/usr`.
fn is_under_usr(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/usr/") else {
        return false;
    };
    rest.split('/').all(|c| !matches!(c, "" | "." | ".."))
}
