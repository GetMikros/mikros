//! The device path and load options an image is started with.

use crate::detect::{END_ENTIRE, MEDIA, MEDIA_FILE_PATH, nodes};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Encodes `text` as a null-terminated UCS-2 string. Returns `None` when it
/// holds a null character or one that UCS-2 cannot represent.
pub fn ucs2(text: &str) -> Option<Vec<u16>> {
    let mut out = Vec::with_capacity(text.len() + 1);
    for c in text.chars() {
        let unit = u16::try_from(u32::from(c)).ok()?;
        if unit == 0 {
            return None;
        }
        out.push(unit);
    }
    out.push(0);
    Some(out)
}

/// The load options of an image started with `args`, as a null-terminated
/// UCS-2 string. `path` names the image in the reason for a failure.
pub fn load_options(path: &str, args: &str) -> Result<Vec<u16>, String> {
    ucs2(args).ok_or_else(|| format!("{path}: invalid args"))
}

/// The device path of the file at `path` on the volume with the device path
/// `volume`: the nodes of the first instance of `volume`, a file path node
/// holding `path`, and an end node.
pub fn file_device_path(volume: &[u8], path: &str) -> Result<Vec<u8>, String> {
    let file = ucs2(path).ok_or_else(|| format!("{path}: invalid path"))?;
    let invalid = || format!("{path}: invalid device path");
    let mut out = Vec::new();
    for (kind, sub, data) in nodes(volume).ok_or_else(invalid)? {
        out.extend_from_slice(&[kind, sub]);
        out.extend_from_slice(&(data.len() as u16 + 4).to_le_bytes());
        out.extend_from_slice(data);
    }
    let len = u16::try_from(file.len() * 2 + 4).map_err(|_| invalid())?;
    out.extend_from_slice(&[MEDIA, MEDIA_FILE_PATH]);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend(file.iter().flat_map(|unit| unit.to_le_bytes()));
    out.extend_from_slice(&END_ENTIRE);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect;
    use crate::entries::normalize_path;
    use alloc::vec;

    #[test]
    fn encodes_load_options_as_null_terminated_ucs2() {
        assert_eq!(
            load_options("\\a.efi", "quiet é"),
            Ok(vec![
                u16::from(b'q'),
                u16::from(b'u'),
                u16::from(b'i'),
                u16::from(b'e'),
                u16::from(b't'),
                u16::from(b' '),
                0xe9,
                0
            ])
        );
        assert_eq!(
            load_options("\\a.efi", "x\u{1f600}"),
            Err(String::from("\\a.efi: invalid args")),
            "outside UCS-2"
        );
        assert_eq!(ucs2("a\0b"), None, "null character");
    }

    #[test]
    fn appends_the_file_to_the_volume() {
        // A PCI node and a hard drive node, then an end node.
        let mut volume = vec![1, 1, 6, 0, 2, 0];
        volume.extend_from_slice(&[4, 1, 8, 0, 9, 9, 9, 9]);
        volume.extend_from_slice(&END_ENTIRE);
        for written in ["/EFI/tools/Shell.efi", "EFI\\tools\\Shell.efi"] {
            let path = normalize_path(written);
            let full = file_device_path(&volume, &path).unwrap();
            assert_eq!(full[..volume.len() - 4], volume[..volume.len() - 4]);
            assert_eq!(
                detect::file_path(&full).as_deref(),
                Some("\\EFI\\tools\\Shell.efi"),
                "{written}"
            );
            assert!(full.ends_with(&END_ENTIRE));
        }
    }

    #[test]
    fn rejects_paths_it_cannot_encode() {
        let volume = END_ENTIRE.to_vec();
        assert_eq!(
            file_device_path(&volume, "\\\u{1f600}.efi"),
            Err(String::from("\\\u{1f600}.efi: invalid path"))
        );
        assert_eq!(
            file_device_path(&[1, 1], "\\a.efi"),
            Err(String::from("\\a.efi: invalid device path"))
        );
    }
}
