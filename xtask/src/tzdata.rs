//! The IANA time zone database, compiled with the host's `zic` into the
//! TZif files that the system keeps in `/usr/share/zoneinfo`.

use anyhow::{Context, Result, bail};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The release's files of zone and link definitions that are compiled.
const SOURCES: [&str; 10] = [
    "africa",
    "antarctica",
    "asia",
    "australasia",
    "europe",
    "northamerica",
    "southamerica",
    "etcetera",
    "backward",
    "factory",
];

/// The directory of the compiled zones, compiling them unless they
/// already are.
pub fn zoneinfo() -> Result<PathBuf> {
    let dir = crate::workspace_root().join("target/tzdata");
    let port = crate::ports::load("tzdata")?;
    let release = port.source.as_str();
    let archive = dir.join(format!("{release}.tar.gz"));
    crate::fetch::cached(
        &format!("{release} database"),
        &port.url,
        &port.sha256,
        &archive,
    )?;
    let out = dir.join(release).join("zoneinfo");
    let stamp = dir.join(release).join(".mikros-compiled");
    if stamp.exists() {
        return Ok(out);
    }
    let source = dir.join(release).join("source");
    for d in [&source, &out] {
        if d.exists() {
            fs::remove_dir_all(d).with_context(|| format!("removing {}", d.display()))?;
        }
    }
    fs::create_dir_all(&source)?;
    println!("extracting {}", archive.display());
    let gz = flate2::read::GzDecoder::new(File::open(&archive)?);
    tar::Archive::new(gz)
        .unpack(&source)
        .with_context(|| format!("extracting {}", archive.display()))?;
    // Every transition up to 2037 is written out, and the footer's rule
    // covers the time after.
    let status = Command::new("zic")
        .args(["-b", "fat", "-d"])
        .arg(&out)
        .args(SOURCES)
        .current_dir(&source)
        .status()
        .context("failed to run zic, which comes with the C library's tools (libc-bin)")?;
    if !status.success() {
        bail!("zic failed to compile {release}");
    }
    File::create(&stamp)?;
    Ok(out)
}

/// The compiled zones as members of an image: each file of `zoneinfo`
/// under `usr/share/zoneinfo`.
pub fn members(zoneinfo: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut members = Vec::new();
    let mut pending = vec![zoneinfo.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let inside = path.strip_prefix(zoneinfo)?.to_string_lossy().into_owned();
            members.push((format!("usr/share/zoneinfo/{inside}"), path));
        }
    }
    members.sort();
    Ok(members)
}
