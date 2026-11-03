//! Checking that the built bootloader cannot use the MP services
//! protocol.
//!
//! A UEFI application finds a protocol by passing its GUID to the
//! firmware, so a loader that uses the protocol holds the 16 bytes of
//! its GUID. The check also looks for the GUID of the graphics output
//! protocol, which the loader uses, to be sure that it would find a GUID
//! that is there.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;

/// The bytes of a GUID as the firmware takes it: the first three fields
/// little-endian, followed by the last eight bytes.
const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let (a, b, c) = (a.to_le_bytes(), b.to_le_bytes(), c.to_le_bytes());
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6],
        d[7],
    ]
}

/// `EFI_MP_SERVICES_PROTOCOL`, 3fdda605-a76e-4f46-ad29-12f4531b3d08.
const MP_SERVICES: [u8; 16] = guid(
    0x3fdd_a605,
    0xa76e,
    0x4f46,
    [0xad, 0x29, 0x12, 0xf4, 0x53, 0x1b, 0x3d, 0x08],
);

/// `EFI_GRAPHICS_OUTPUT_PROTOCOL`, 9042a9de-23dc-4a38-96fb-7aded080516a.
const GRAPHICS_OUTPUT: [u8; 16] = guid(
    0x9042_a9de,
    0x23dc,
    0x4a38,
    [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a],
);

fn contains(image: &[u8], bytes: &[u8]) -> bool {
    image.windows(bytes.len()).any(|w| w == bytes)
}

/// What is wrong with the loader `image`, if anything.
fn problem(image: &[u8]) -> Option<&'static str> {
    if !contains(image, &GRAPHICS_OUTPUT) {
        return Some("holds no graphics output protocol GUID, so GUIDs cannot be found in it");
    }
    if contains(image, &MP_SERVICES) {
        return Some("holds the MP services protocol GUID");
    }
    None
}

/// Fails when the loader at `loader` holds the MP services protocol's
/// GUID.
pub fn check(loader: &Path) -> Result<()> {
    let image = fs::read(loader).with_context(|| format!("reading {}", loader.display()))?;
    if let Some(problem) = problem(&image) {
        bail!("{} {problem}", loader.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_guids_as_the_firmware_does() {
        assert_eq!(
            GRAPHICS_OUTPUT,
            [
                0xde, 0xa9, 0x42, 0x90, 0xdc, 0x23, 0x38, 0x4a, 0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80,
                0x51, 0x6a
            ]
        );
        assert_eq!(MP_SERVICES[..4], [0x05, 0xa6, 0xdd, 0x3f]);
        assert_eq!(
            MP_SERVICES[8..],
            [0xad, 0x29, 0x12, 0xf4, 0x53, 0x1b, 0x3d, 0x08]
        );
    }

    #[test]
    fn refuses_a_loader_with_the_mp_services_guid() {
        let mut image = vec![0u8; 100];
        image[10..26].copy_from_slice(&GRAPHICS_OUTPUT);
        assert_eq!(problem(&image), None);
        image[61..77].copy_from_slice(&MP_SERVICES);
        assert!(problem(&image).unwrap().contains("MP services"));
    }

    #[test]
    fn needs_the_graphics_output_guid() {
        let image = vec![0u8; 100];
        assert!(problem(&image).unwrap().contains("graphics output"));
    }
}
