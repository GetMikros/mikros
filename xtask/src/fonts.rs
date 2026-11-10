//! The system fonts: DejaVu Sans, the proportional family, and DejaVu
//! Sans Mono, the monospaced one, each regular and bold, from DejaVu's
//! release archive pinned by its SHA-256, with their license.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;

const URL: &str = "https://github.com/dejavu-fonts/dejavu-fonts/releases/download/version_2_37/dejavu-fonts-ttf-2.37.tar.bz2";
const SHA256: &str = "fa9ca4d13871dd122f61258a80d01751d603b4d3ee14095d65453b4e846e17d7";
const SOURCE: &str = "dejavu-fonts-ttf-2.37";
/// The font files the system installs.
pub const FILES: [&str; 4] = [
    "DejaVuSans.ttf",
    "DejaVuSans-Bold.ttf",
    "DejaVuSansMono.ttf",
    "DejaVuSansMono-Bold.ttf",
];

/// Where the fonts were unpacked.
pub struct Fonts {
    /// The directory of the font files.
    pub ttf: PathBuf,
    pub license: PathBuf,
}

/// Fetches and unpacks the fonts.
pub fn fetch() -> Result<Fonts> {
    let dir = crate::workspace_root().join("target/sources/dejavu");
    let archive = dir.join(format!("{SOURCE}.tar.bz2"));
    crate::fetch::cached("DejaVu fonts", URL, SHA256, &archive)?;
    let source = dir.join(SOURCE);
    let ttf = source.join("ttf");
    if !FILES.iter().all(|f| ttf.join(f).exists()) {
        // The archive is bzip2-compressed, which the host's tar unpacks.
        let status = Command::new("tar")
            .arg("-xjf")
            .arg(&archive)
            .arg("-C")
            .arg(&dir)
            .status()
            .context("failed to run tar")?;
        if !status.success() {
            bail!("unpacking {} failed", archive.display());
        }
    }
    for file in FILES {
        if !ttf.join(file).exists() {
            bail!("{} has no ttf/{file}", archive.display());
        }
    }
    Ok(Fonts {
        ttf,
        license: source.join("LICENSE"),
    })
}
