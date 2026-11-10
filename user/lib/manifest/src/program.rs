//! Reading program manifests: those of system programs that are neither
//! services nor drivers, in `/usr/lib/programs/<app-id>.yaml`.

use crate::{Error, ErrorKind, SUFFIX, Unknown, error, is_under_usr, list, strings, unknown_keys};
use alloc::string::String;
use alloc::vec::Vec;

/// The keys of a program manifest.
const KEYS: &[&str] = &["app_id", "binary", "permissions", "namespace", "spawned_by"];
/// The keys of an item of `namespace`.
const NAMESPACE_KEYS: &[&str] = &["path", "rights"];
/// The prefix of the app IDs of system programs, and the app IDs that no
/// program manifest may name.
const SYSTEM_PREFIX: &str = "system.";
const CLI: &str = "system.cli";
const DRIVER_PREFIX: &str = "system.driver.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub app_id: String,
    pub binary: String,
    pub permissions: Vec<String>,
    pub namespace: Vec<NamespaceEntry>,
    /// The app IDs of the processes that may start the program with its
    /// app ID.
    pub spawned_by: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceEntry {
    pub path: String,
    pub rights: String,
}

/// A program manifest, and the keys in it that it does not know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramManifest {
    pub program: Program,
    pub unknown: Vec<Unknown>,
}

/// Reads the program manifest in `bytes`, from the file `file_name`.
/// Whether the app ID is also that of a service is for the caller to
/// check.
pub fn parse_program(file_name: &str, bytes: &[u8]) -> Result<ProgramManifest, Error> {
    let root = mikros_yaml::parse(bytes)?;
    let map = root.map()?;
    let mut unknown = unknown_keys(map, KEYS);
    let required = |key| map.get(key).ok_or(error(&root, ErrorKind::Missing(key)));
    let app_id_node = required("app_id")?;
    let app_id = app_id_node.string()?;
    if !app_id.starts_with(SYSTEM_PREFIX) || app_id == CLI || app_id.starts_with(DRIVER_PREFIX) {
        return Err(error(app_id_node, ErrorKind::BadAppId));
    }
    if file_name.strip_suffix(SUFFIX) != Some(app_id) {
        return Err(error(app_id_node, ErrorKind::FileName));
    }
    let binary_node = required("binary")?;
    let binary = binary_node.string()?;
    if !is_under_usr(binary) {
        return Err(error(binary_node, ErrorKind::BadBinary));
    }
    let namespace = namespace(map, &mut unknown)?;
    let program = Program {
        app_id: app_id.into(),
        binary: binary.into(),
        permissions: crate::permissions(map)?,
        namespace,
        spawned_by: strings(map, "spawned_by")?,
    };
    Ok(ProgramManifest { program, unknown })
}

/// The entries of `namespace` in `map`, none when it is absent, adding
/// the keys of entries it does not know to `unknown`.
pub(crate) fn namespace(
    map: mikros_yaml::Map<'_>,
    unknown: &mut Vec<Unknown>,
) -> Result<Vec<NamespaceEntry>, Error> {
    let mut namespace = Vec::new();
    for item in list(map, "namespace")? {
        let entry = item.map()?;
        unknown.extend(unknown_keys(entry, NAMESPACE_KEYS));
        let field = |key| {
            entry
                .get(key)
                .ok_or(error(item, ErrorKind::Missing(key)))
                .and_then(|n| n.string().map_err(Error::from))
        };
        namespace.push(NamespaceEntry {
            path: field("path")?.into(),
            rights: field("rights")?.into(),
        });
    }
    Ok(namespace)
}
