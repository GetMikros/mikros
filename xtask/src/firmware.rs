//! The pinned OVMF build that QEMU boots with.

use anyhow::{Context, Result, bail};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

/// The OVMF release, from github.com/rust-osdev/ovmf-prebuilt.
const RELEASE: &str = "edk2-stable202605-r1";
const URL: &str = "https://github.com/rust-osdev/ovmf-prebuilt/releases/download/\
                   edk2-stable202605-r1/edk2-stable202605-r1-bin.tar.xz";
/// SHA-256 of the release archive at `URL`.
const SHA256: &str = "8ae4d2d73161cc2335f5675d3b8b6edfa0642301679764a246940488ea3ce20d";
/// The files taken from the archive, by their path inside it.
const FILES: [&str; 2] = ["x64/code.fd", "x64/vars.fd"];

/// Paths of the extracted firmware files.
pub struct Firmware {
    pub code: PathBuf,
    pub vars: PathBuf,
}

fn firmware_dir() -> PathBuf {
    crate::workspace_root().join("target/firmware")
}

/// Where the firmware files are after `fetch` has succeeded.
pub fn paths() -> Firmware {
    let dir = firmware_dir().join(RELEASE);
    Firmware {
        code: dir.join("code.fd"),
        vars: dir.join("vars.fd"),
    }
}

/// Downloads the release archive unless a copy with the right checksum is
/// already present, then extracts the firmware files from it.
pub fn fetch() -> Result<()> {
    let dir = firmware_dir();
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let archive = dir.join(format!("{RELEASE}.tar.xz"));

    crate::fetch::cached(&format!("OVMF {RELEASE}"), URL, SHA256, &archive)?;

    let firmware = paths();
    extract(&archive, &firmware)
}

fn extract(archive: &Path, firmware: &Firmware) -> Result<()> {
    let mut xz = BufReader::new(File::open(archive)?);
    let mut tarball = Vec::new();
    lzma_rs::xz_decompress(&mut xz, &mut tarball)
        .with_context(|| format!("failed to decompress {}", archive.display()))?;

    let destinations = [&firmware.code, &firmware.vars];
    let wanted: Vec<String> = FILES
        .iter()
        .map(|file| format!("{RELEASE}-bin/{file}"))
        .collect();
    let mut found = [false; FILES.len()];

    for entry in tar::Archive::new(tarball.as_slice()).entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let Some(index) = wanted.iter().position(|w| *w == path) else {
            continue;
        };
        let destination = destinations[index];
        fs::create_dir_all(destination.parent().unwrap())?;
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents)?;
        fs::write(destination, contents)
            .with_context(|| format!("failed to write {}", destination.display()))?;
        found[index] = true;
    }

    if let Some(index) = found.iter().position(|f| !f) {
        bail!("{} does not contain {}", archive.display(), wanted[index]);
    }
    Ok(())
}
