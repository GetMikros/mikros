//! Reading driver manifests: the `driver.yaml` in each driver's own
//! directory under `/system/drivers/`. A driver of the system's is in
//! `system.driver.<name>/` and has the app ID of that name; an installed
//! driver is in `<id>/`, named and with the app ID `<id>`, its package's
//! ID (MIKSPEC-0025.R72, MIKSPEC-0101.R76). An installed driver may not
//! need `bus` or `ioports`, may match only the IDs that
//! `installed_may_match` accepts, and is never chosen for a device with
//! a protected interface class (MIKSPEC-0025.R69).

use crate::program::namespace;
use crate::{
    Error, ErrorKind, NamespaceEntry, PRIORITY_DEFAULT, Unknown, error, is_kebab_case,
    is_under_system, list, strings, unknown_keys,
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
    "boot",
];
/// The keys of service manifests that a driver manifest must not have.
const SERVICE_KEYS: [&str; 5] = ["provides", "after", "start", "restart", "boot_handles"];
/// The permissions that no driver manifest may list.
const DRIVER_RESERVED_PERMISSIONS: [&str; 1] = ["system.log_sources.create"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Driver {
    pub name: String,
    /// The app ID it runs with.
    pub app_id: String,
    /// Whether the bootloader loads it as a boot module, for an installed
    /// driver (MIKSPEC-0101.R20).
    pub boot: bool,
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

/// Whether `id` is a package ID: two or more parts separated by `.`, each
/// a lowercase ASCII letter followed by lowercase ASCII letters, digits
/// and `_`, at most 128 bytes in all, and not one of the system's.
pub fn is_package_id(id: &str) -> bool {
    let part = |p: &str| {
        let mut chars = p.chars();
        chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    id.len() <= 128
        && id.split('.').count() >= 2
        && id.split('.').all(part)
        && !id.starts_with("system.")
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

/// The starts of the hardware IDs of USB interfaces of the classes HID,
/// mass storage and hub, which installed drivers never drive.
const PROTECTED: [&str; 3] = ["usb:c03", "usb:c08", "usb:c09"];

/// The forms of the IDs that an installed driver may match, each a list
/// of literal parts with the number of lowercase hexadecimal digits that
/// follow each: a PCI function's vendor and device, alone, with its
/// subsystem's, and with those and its revision; and a USB interface's
/// vendor, product and interface number.
const INSTALLED_FORMS: [&[(&str, usize)]; 4] = [
    &[("pci:v", 4), ("d", 4)],
    &[("pci:v", 4), ("d", 4), ("sv", 4), ("sd", 4)],
    &[("pci:v", 4), ("d", 4), ("sv", 4), ("sd", 4), ("r", 2)],
    &[("usb:v", 4), ("p", 4), ("i", 2)],
];

/// Whether an installed driver may have `id` in its `match` list: one of
/// `INSTALLED_FORMS` exactly, never a class ID or another bus's.
pub fn installed_may_match(id: &str) -> bool {
    INSTALLED_FORMS.iter().any(|form| has_form(id, form))
}

fn has_form(mut id: &str, form: &[(&str, usize)]) -> bool {
    let hex = |b: &u8| b.is_ascii_digit() || (b'a'..=b'f').contains(b);
    for &(literal, digits) in form {
        let Some(rest) = id.strip_prefix(literal) else {
            return false;
        };
        if rest.len() < digits || !rest.as_bytes()[..digits].iter().all(hex) {
            return false;
        }
        id = &rest[digits..];
    }
    id.is_empty()
}

impl Driver {
    /// Whether the driver was installed from a package: its name is a
    /// package ID rather than a one-word name.
    pub fn is_installed(&self) -> bool {
        is_package_id(&self.name)
    }
}

/// The driver among `drivers` for a device with `hardware_ids`, from
/// the most specific to the least: the `Normal` driver matching the
/// first ID that one matches, else the `Fallback` driver chosen so.
/// Of several that match the same ID, a driver of the system's comes
/// before an installed one, and then the one whose name sorts first.
/// Installed drivers are passed over for a device with an ID that
/// starts as one of `PROTECTED`. None when no driver matches.
pub fn choose<'a>(drivers: &'a [Driver], hardware_ids: &'a [String]) -> Option<Choice<'a>> {
    let protected = hardware_ids
        .iter()
        .any(|id| PROTECTED.iter().any(|p| id.starts_with(p)));
    [DriverKind::Normal, DriverKind::Fallback]
        .into_iter()
        .find_map(|kind| {
            hardware_ids.iter().find_map(|id| {
                let mut matching: Vec<&Driver> = drivers
                    .iter()
                    .filter(|d| d.kind == kind && d.matches.contains(id))
                    .filter(|d| !(protected && d.is_installed()))
                    .collect();
                matching
                    .sort_by(|a, b| (a.is_installed(), &a.name).cmp(&(b.is_installed(), &b.name)));
                let (driver, others) = matching.split_first()?;
                Some(Choice {
                    driver,
                    hardware_id: id,
                    passed_over: others.iter().map(|d| d.name.as_str()).collect(),
                })
            })
        })
}

/// The prefix of the app IDs of the system's drivers, which name their
/// folders in `/system/drivers/`.
pub const DRIVER_PREFIX: &str = "system.driver.";

/// Reads the driver manifest in `bytes`, from the folder `directory`,
/// which must be `system.driver.<name>`.
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
    let app_id = match directory.strip_prefix(DRIVER_PREFIX) {
        Some(own) if is_kebab_case(name) && own == name => String::from(directory),
        Some(_) if !is_kebab_case(name) => return Err(error(name_node, ErrorKind::BadName)),
        Some(_) => return Err(error(name_node, ErrorKind::FileName)),
        // A system driver's name outside its folder.
        None if is_kebab_case(name) => return Err(error(name_node, ErrorKind::FileName)),
        // An installed driver, named as its package.
        None if !is_package_id(name) => return Err(error(name_node, ErrorKind::BadName)),
        None if directory != name => return Err(error(name_node, ErrorKind::FileName)),
        None => String::from(name),
    };
    let binary_node = required("binary")?;
    let binary = binary_node.string()?;
    if !is_under_system(binary) {
        return Err(error(binary_node, ErrorKind::BadBinary));
    }
    let mut matches = Vec::new();
    // A folder other than `system.driver.<name>` holds an installed
    // driver, which the name checks above made sure is named by its
    // package ID.
    let installed = directory.strip_prefix(DRIVER_PREFIX).is_none();
    for item in list(map, "match")? {
        let id = item.string()?;
        if !is_hardware_id(id) {
            return Err(error(item, ErrorKind::BadHardwareId));
        }
        if installed && !installed_may_match(id) {
            return Err(error(item, ErrorKind::InstalledMatch));
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
        if installed && matches!(needs.last(), Some(Need::Ioports | Need::Bus)) {
            return Err(error(item, ErrorKind::InstalledNeed));
        }
    }
    let priority = match map.get("priority") {
        None => PRIORITY_DEFAULT,
        Some(node) => node.integer(i64::MIN, i64::MAX)?,
    };
    for item in list(map, "permissions")? {
        if DRIVER_RESERVED_PERMISSIONS.contains(&item.string()?) {
            return Err(error(item, ErrorKind::ReservedPermission));
        }
        if item.string()?.starts_with("user.") {
            return Err(error(item, ErrorKind::BadScope));
        }
    }
    let boot = match map.get("boot") {
        None => false,
        Some(node) => node.boolean()?,
    };
    let driver = Driver {
        name: name.into(),
        app_id,
        boot,
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
