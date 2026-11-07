//! Reading driver manifests: the `driver.yaml` in each driver's own
//! directory under `/usr/lib/drivers/`.

use crate::program::namespace;
use crate::{
    Error, ErrorKind, NamespaceEntry, PRIORITY_DEFAULT, Unknown, error, is_kebab_case,
    is_under_usr, list, strings, unknown_keys,
};
use alloc::string::String;
use alloc::vec::Vec;

/// The keys of a driver manifest.
const KEYS: &[&str] = &[
    "name",
    "binary",
    "permissions",
    "namespace",
    "priority",
    "match",
    "kind",
    "needs",
];
/// The keys of service manifests that a driver manifest must not have.
const SERVICE_KEYS: [&str; 5] = ["provides", "after", "start", "restart", "boot_handles"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Driver {
    pub name: String,
    pub binary: String,
    pub permissions: Vec<String>,
    pub namespace: Vec<NamespaceEntry>,
    pub priority: i64,
    /// The hardware IDs it matches.
    pub matches: Vec<String>,
    pub kind: DriverKind,
    pub needs: Vec<Need>,
}

/// Whether a driver is chosen only when no `Normal` one matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverKind {
    Normal,
    Fallback,
}

/// Hardware access a driver needs beyond its device's resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    Dma,
    Ioports,
    Bus,
}

/// A valid driver manifest, and the keys in it that it does not know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverManifest {
    pub driver: Driver,
    pub unknown: Vec<Unknown>,
}

/// Whether `id` is a hardware ID: lowercase ASCII letters, digits, `:`,
/// `-` and `_`, with a bus name of at least one of them before the
/// first `:`.
pub fn is_hardware_id(id: &str) -> bool {
    let allowed = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b":-_".contains(&b);
    let has_bus = id.split_once(':').is_some_and(|(bus, _)| !bus.is_empty());
    has_bus && id.bytes().all(allowed)
}

/// The driver chosen for a device: the driver, the hardware ID of the
/// device that it matched, and the names of the other drivers of its
/// kind that match that ID, which sort after its own.
#[derive(Debug, PartialEq, Eq)]
pub struct Choice<'a> {
    pub driver: &'a Driver,
    pub hardware_id: &'a str,
    pub passed_over: Vec<&'a str>,
}

/// The driver among `drivers` for a device with `hardware_ids`, from
/// the most specific to the least: the `Normal` driver matching the
/// first ID that one matches, else the `Fallback` driver chosen so,
/// and of several that match the same ID, the one whose name sorts
/// first. None when no driver matches.
pub fn choose<'a>(drivers: &'a [Driver], hardware_ids: &'a [String]) -> Option<Choice<'a>> {
    [DriverKind::Normal, DriverKind::Fallback]
        .into_iter()
        .find_map(|kind| {
            hardware_ids.iter().find_map(|id| {
                let mut matching: Vec<&Driver> = drivers
                    .iter()
                    .filter(|d| d.kind == kind && d.matches.contains(id))
                    .collect();
                matching.sort_by(|a, b| a.name.cmp(&b.name));
                let (driver, others) = matching.split_first()?;
                Some(Choice {
                    driver,
                    hardware_id: id,
                    passed_over: others.iter().map(|d| d.name.as_str()).collect(),
                })
            })
        })
}

/// Reads the driver manifest in `bytes`, from the directory `directory`.
pub fn parse_driver(directory: &str, bytes: &[u8]) -> Result<DriverManifest, Error> {
    let root = mikros_yaml::parse(bytes)?;
    let map = root.map()?;
    if let Some(key) = SERVICE_KEYS.into_iter().find(|&k| map.get(k).is_some()) {
        let node = map.get(key).expect("found");
        return Err(error(node, ErrorKind::ServiceKey(key)));
    }
    let mut unknown = unknown_keys(map, KEYS);
    let required = |key| map.get(key).ok_or(error(&root, ErrorKind::Missing(key)));
    let name_node = required("name")?;
    let name = name_node.string()?;
    if !is_kebab_case(name) {
        return Err(error(name_node, ErrorKind::BadName));
    }
    if directory != name {
        return Err(error(name_node, ErrorKind::FileName));
    }
    let binary_node = required("binary")?;
    let binary = binary_node.string()?;
    if !is_under_usr(binary) {
        return Err(error(binary_node, ErrorKind::BadBinary));
    }
    let mut matches = Vec::new();
    for item in list(map, "match")? {
        let id = item.string()?;
        if !is_hardware_id(id) {
            return Err(error(item, ErrorKind::BadHardwareId));
        }
        matches.push(id.into());
    }
    let kind = match map.get("kind") {
        None => DriverKind::Normal,
        Some(node) => match node.string()? {
            "normal" => DriverKind::Normal,
            "fallback" => DriverKind::Fallback,
            _ => return Err(error(node, ErrorKind::BadKind)),
        },
    };
    let mut needs = Vec::new();
    for item in list(map, "needs")? {
        needs.push(match item.string()? {
            "dma" => Need::Dma,
            "ioports" => Need::Ioports,
            "bus" => Need::Bus,
            _ => return Err(error(item, ErrorKind::BadNeed)),
        });
    }
    let priority = match map.get("priority") {
        None => PRIORITY_DEFAULT,
        Some(node) => node.integer(i64::MIN, i64::MAX)?,
    };
    let driver = Driver {
        name: name.into(),
        binary: binary.into(),
        permissions: strings(map, "permissions")?,
        namespace: namespace(map, &mut unknown)?,
        priority,
        matches,
        kind,
        needs,
    };
    Ok(DriverManifest { driver, unknown })
}
