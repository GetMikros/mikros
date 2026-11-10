//! The pinned OVMF builds that QEMU boots with: the usual one, and a
//! Secure Boot one for signed images.

use anyhow::{Context, Result, bail};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

/// A pinned firmware download.
struct Release {
    name: &'static str,
    url: &'static str,
    /// SHA-256 of the download at `url`.
    sha256: &'static str,
    /// The firmware code and variable store, by their paths in the tar
    /// archive.
    files: [&'static str; 2],
    /// Whether the download is a Debian package holding the tar archive,
    /// rather than the xz-compressed archive itself.
    deb: bool,
}

/// The OVMF release, from github.com/rust-osdev/ovmf-prebuilt.
const RELEASE: Release = Release {
    name: "edk2-stable202605-r1",
    url: "https://github.com/rust-osdev/ovmf-prebuilt/releases/download/\
          edk2-stable202605-r1/edk2-stable202605-r1-bin.tar.xz",
    sha256: "8ae4d2d73161cc2335f5675d3b8b6edfa0642301679764a246940488ea3ce20d",
    files: [
        "edk2-stable202605-r1-bin/x64/code.fd",
        "edk2-stable202605-r1-bin/x64/vars.fd",
    ],
    deb: false,
};

/// The Secure Boot build of OVMF, with an empty variable store, from
/// Debian's package ovmf 2025.02-8+deb13u1 on snapshot.debian.org.
const SECURE_BOOT_RELEASE: Release = Release {
    name: "ovmf-2025.02-8+deb13u1",
    url: "https://snapshot.debian.org/file/e22ebb3da0cbf61fb9b0fefa2d7f79d08c878885",
    sha256: "78e0d54df11fc77406cb7a0bc9a39e5bca6d1cbe06556b91d9a73491c52decdf",
    files: [
        "./usr/share/OVMF/OVMF_CODE_4M.secboot.fd",
        "./usr/share/OVMF/OVMF_VARS_4M.fd",
    ],
    deb: true,
};

/// Paths of the extracted firmware files.
pub struct Firmware {
    pub code: PathBuf,
    pub vars: PathBuf,
    /// Whether this is the Secure Boot build, which needs System
    /// Management Mode and a secure flash.
    pub secure_boot: bool,
}

fn firmware_dir() -> PathBuf {
    crate::workspace_root().join("target/firmware")
}

fn release_paths(release: &Release) -> Firmware {
    let dir = firmware_dir().join(release.name);
    Firmware {
        code: dir.join("code.fd"),
        vars: dir.join("vars.fd"),
        secure_boot: release.deb,
    }
}

/// Where the firmware files are after `fetch` has succeeded.
pub fn paths() -> Firmware {
    release_paths(&RELEASE)
}

/// Where the files of the Secure Boot build are after `fetch` has
/// succeeded.
pub fn secure_boot_paths() -> Firmware {
    release_paths(&SECURE_BOOT_RELEASE)
}

/// Downloads each release unless a copy with the right checksum is
/// already present, then extracts the firmware files from it.
pub fn fetch() -> Result<()> {
    for release in [&RELEASE, &SECURE_BOOT_RELEASE] {
        let dir = firmware_dir();
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        let extension = if release.deb { "deb" } else { "tar.xz" };
        let download = dir.join(format!("{}.{extension}", release.name));
        let what = format!("OVMF {}", release.name);
        crate::fetch::cached(&what, release.url, release.sha256, &download)?;
        extract(release, &download)?;
    }
    Ok(())
}

/// The member `name` of the ar archive `archive`, as Debian packages are.
fn ar_member<'a>(archive: &'a [u8], name: &str) -> Result<&'a [u8]> {
    let Some(mut rest) = archive.strip_prefix(b"!<arch>\n") else {
        bail!("not an ar archive");
    };
    while rest.len() >= 60 {
        let (header, after) = rest.split_at(60);
        let size: usize = std::str::from_utf8(&header[48..58])?.trim().parse()?;
        if after.len() < size {
            break;
        }
        let member = std::str::from_utf8(&header[..16])?.trim_end();
        if member.trim_end_matches('/') == name {
            return Ok(&after[..size]);
        }
        rest = &after[(size + size % 2).min(after.len())..];
    }
    bail!("the archive has no member {name}")
}

fn extract(release: &Release, download: &Path) -> Result<()> {
    let mut contents = Vec::new();
    File::open(download)?.read_to_end(&mut contents)?;
    let compressed = if release.deb {
        ar_member(&contents, "data.tar.xz")
            .with_context(|| format!("reading {}", download.display()))?
    } else {
        &contents
    };
    let mut tarball = Vec::new();
    lzma_rs::xz_decompress(&mut BufReader::new(compressed), &mut tarball)
        .with_context(|| format!("failed to decompress {}", download.display()))?;

    let firmware = release_paths(release);
    let destinations = [&firmware.code, &firmware.vars];
    let mut found = [false; 2];

    for entry in tar::Archive::new(tarball.as_slice()).entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let Some(index) = release.files.iter().position(|w| *w == path) else {
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
        bail!(
            "{} does not contain {}",
            download.display(),
            release.files[index]
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_members_of_ar_archives() {
        let mut archive = b"!<arch>\n".to_vec();
        for (name, data) in [("debian-binary", &b"2.0\n"[..]), ("data.tar.xz", b"xyz")] {
            archive
                .extend_from_slice(format!("{name:<16}{:<32}{:<10}`\n", "", data.len()).as_bytes());
            archive.extend_from_slice(data);
            if data.len() % 2 == 1 {
                archive.push(b'\n');
            }
        }
        assert_eq!(ar_member(&archive, "data.tar.xz").unwrap(), b"xyz");
        assert_eq!(ar_member(&archive, "debian-binary").unwrap(), b"2.0\n");
        assert!(ar_member(&archive, "control.tar.xz").is_err());
    }
}
