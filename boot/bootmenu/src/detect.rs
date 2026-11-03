//! Finding other systems to add to the menu: reading firmware load options,
//! recognizing Mikros boot files, and skipping files that an earlier entry
//! already starts.

use crate::entries::normalize_path;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Images looked for on the boot volume, with the names of their entries.
#[cfg(not(target_arch = "aarch64"))]
pub const KNOWN_IMAGES: [(&str, &str); 3] = [
    ("\\EFI\\Microsoft\\Boot\\bootmgfw.efi", "Windows"),
    ("\\EFI\\systemd\\systemd-bootx64.efi", "Linux"),
    ("\\EFI\\refind\\refind_x64.efi", "rEFInd"),
];
#[cfg(target_arch = "aarch64")]
pub const KNOWN_IMAGES: [(&str, &str); 3] = [
    ("\\EFI\\Microsoft\\Boot\\bootmgfw.efi", "Windows"),
    ("\\EFI\\systemd\\systemd-bootaa64.efi", "Linux"),
    ("\\EFI\\refind\\refind_aa64.efi", "rEFInd"),
];

/// The removable-media fallback path.
#[cfg(not(target_arch = "aarch64"))]
pub const FALLBACK_PATH: &str = "\\EFI\\BOOT\\BOOTX64.EFI";
#[cfg(target_arch = "aarch64")]
pub const FALLBACK_PATH: &str = "\\EFI\\BOOT\\BOOTAA64.EFI";

/// The folder that holds the Mikros boot files.
const MIKROS_DIR: &str = "\\EFI\\Mikros\\";

/// The PE section that marks the boot manager image.
pub const MIKROS_SECTION: &[u8; 8] = b".mikros\0";

/// Device path node types and subtypes used here.
pub(crate) const MEDIA: u8 = 4;
pub(crate) const MEDIA_FILE_PATH: u8 = 4;
const END: u8 = 0x7f;
pub(crate) const END_ENTIRE: [u8; 4] = [0x7f, 0xff, 4, 0];

/// A firmware load option (`Boot####` variable).
#[derive(Debug, PartialEq, Eq)]
pub struct LoadOption {
    pub description: String,
    /// The first device path of the option, ending with an end node.
    pub device_path: Vec<u8>,
}

fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Decodes a null-terminated or unterminated UCS-2 string.
fn ucs2(data: &[u8]) -> String {
    let units = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .take_while(|&u| u != 0);
    char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// The nodes of the first instance of `device_path`, as (type, subtype,
/// data) up to its first end node. Returns `None` when a node is malformed
/// or the path has no end node.
pub(crate) fn nodes(device_path: &[u8]) -> Option<Vec<(u8, u8, &[u8])>> {
    let mut out = Vec::new();
    let mut offset = 0;
    loop {
        let kind = *device_path.get(offset)?;
        let sub = *device_path.get(offset + 1)?;
        let len = usize::from(u16_at(device_path, offset + 2)?);
        if len < 4 {
            return None;
        }
        let data = device_path.get(offset + 4..offset + len)?;
        if kind == END {
            return Some(out);
        }
        out.push((kind, sub, data));
        offset += len;
    }
}

/// Reads a load option. Returns `None` when it is malformed.
pub fn parse_load_option(data: &[u8]) -> Option<LoadOption> {
    let list_len = usize::from(u16_at(data, 4)?);
    let description_start = 6;
    let description_len = data
        .get(description_start..)?
        .as_chunks::<2>()
        .0
        .iter()
        .position(|c| c == &[0, 0])?
        * 2;
    let description = ucs2(&data[description_start..description_start + description_len]);
    let list_start = description_start + description_len + 2;
    let list = data.get(list_start..list_start + list_len)?;
    let count: usize = nodes(list)?.iter().map(|(_, _, data)| data.len() + 4).sum();
    let mut device_path = list[..count].to_vec();
    device_path.extend_from_slice(&END_ENTIRE);
    Some(LoadOption {
        description,
        device_path,
    })
}

/// The path of the file that `device_path` ends in, normalized as entry
/// paths are, or `None` when its last node is not a file path. Consecutive
/// file path nodes at the end are joined.
pub fn file_path(device_path: &[u8]) -> Option<String> {
    let nodes = nodes(device_path)?;
    let files = nodes
        .iter()
        .rev()
        .take_while(|&&(kind, sub, _)| kind == MEDIA && sub == MEDIA_FILE_PATH)
        .count();
    if files == 0 {
        return None;
    }
    let mut path = String::new();
    for &(_, _, data) in &nodes[nodes.len() - files..] {
        let part = ucs2(data);
        if !path.is_empty() && !path.ends_with(['\\', '/']) && !part.starts_with(['\\', '/']) {
            path.push('\\');
        }
        path.push_str(&part);
    }
    Some(normalize_path(&path))
}

/// Whether two normalized paths name the same file. FAT compares names
/// without regard to ASCII case.
pub fn same_file(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Whether a normalized path is under the Mikros folder.
pub fn in_mikros_dir(path: &str) -> bool {
    path.get(..MIKROS_DIR.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(MIKROS_DIR))
}

/// Whether the PE image `image` has a section named `name`, padded with
/// null bytes to 8 bytes.
pub fn has_pe_section(image: &[u8], name: &[u8; 8]) -> bool {
    let check = || -> Option<bool> {
        if image.get(..2)? != b"MZ" {
            return Some(false);
        }
        let pe = u32_at(image, 0x3c)? as usize;
        if image.get(pe..pe + 4)? != b"PE\0\0" {
            return Some(false);
        }
        let sections = usize::from(u16_at(image, pe + 6)?);
        let optional_size = usize::from(u16_at(image, pe + 20)?);
        let table = pe + 24 + optional_size;
        Some((0..sections).any(|i| image.get(table + 40 * i..table + 40 * i + 8) == Some(name)))
    };
    check().unwrap_or(false)
}

/// The files started by the entries so far, by volume.
pub struct Started<V> {
    files: Vec<(V, String)>,
}

impl<V: PartialEq> Started<V> {
    pub fn new() -> Started<V> {
        Started { files: Vec::new() }
    }

    /// Records an entry that starts `path` on `volume`. Returns `false`, and
    /// records nothing, when an earlier entry already starts that file.
    pub fn insert(&mut self, volume: V, path: &str) -> bool {
        if self.contains(&volume, path) {
            return false;
        }
        self.files.push((volume, String::from(path)));
        true
    }

    pub fn contains(&self, volume: &V, path: &str) -> bool {
        self.files
            .iter()
            .any(|(v, p)| v == volume && same_file(p, path))
    }
}

impl<V: PartialEq> Default for Started<V> {
    fn default() -> Self {
        Started::new()
    }
}

/// The IDs given to detected entries: `detected-1`, `detected-2` and so on,
/// skipping those for which `used` is true.
pub fn ids(used: impl Fn(&str) -> bool) -> impl Iterator<Item = String> {
    (1..)
        .map(|n| format!("detected-{n}"))
        .filter(move |id| !used(id))
}

#[cfg(test)]
mod tests;
