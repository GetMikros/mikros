//! Signed images for UEFI Secure Boot.

pub mod authenticode;
pub mod check;
pub mod der;
pub mod enroll;
pub mod key;
pub mod vars;

use anyhow::{Context, Result};
pub use key::Key;

/// Signs every EFI executable among `files`, the paths and contents of
/// the EFI system partition, with `key`, and adds the enrollment files.
pub fn sign_esp(files: &mut Vec<(String, Vec<u8>)>, key: &Key) -> Result<()> {
    for (path, data) in files.iter_mut() {
        if path.to_ascii_lowercase().ends_with(".efi") {
            *data = authenticode::sign(data, key).with_context(|| format!("signing {path}"))?;
        }
    }
    files.extend(enroll::files(key)?);
    Ok(())
}
