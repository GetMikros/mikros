//! Downloading pinned files and checking their SHA-256 checksums.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Downloads `url` to `path` unless a file with the checksum `sha256` is
/// already there. `what` names the file in the progress output. A file
/// whose checksum matched once is not read again while its size and
/// modification time are those recorded in `<path>.verified`.
pub fn cached(what: &str, url: &str, sha256: &str, path: &Path) -> Result<()> {
    if path.exists() && (verified(path, sha256) || sha256_file(path)? == sha256) {
        record(path, sha256)?;
        println!("ok       {what}");
        return Ok(());
    }
    println!("fetching {what}");
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }
    download(url, sha256, path)?;
    record(path, sha256)
}

/// The file that records that `path` had the checksum it names.
fn record_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".verified");
    PathBuf::from(name)
}

/// The record of `path` with the checksum `sha256`: the checksum, the
/// file's size and its modification time in nanoseconds.
fn record_of(path: &Path, sha256: &str) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(format!("{sha256} {} {}\n", meta.len(), modified.as_nanos()))
}

/// Whether `path` is unchanged since its checksum was found to be
/// `sha256`.
fn verified(path: &Path, sha256: &str) -> bool {
    record_of(path, sha256).is_some_and(|r| fs::read_to_string(record_path(path)).ok() == Some(r))
}

/// Records that `path` has the checksum `sha256`.
fn record(path: &Path, sha256: &str) -> Result<()> {
    let Some(record) = record_of(path, sha256) else {
        return Ok(());
    };
    let to = record_path(path);
    fs::write(&to, record).with_context(|| format!("failed to write {}", to.display()))
}

fn download(url: &str, sha256: &str, path: &Path) -> Result<()> {
    let partial = path.with_extension("part");
    let response = ureq::get(url)
        .call()
        .with_context(|| format!("failed to download {url}"))?;
    let mut body = response.into_body().into_reader();
    let mut file = File::create(&partial)
        .with_context(|| format!("failed to create {}", partial.display()))?;
    io::copy(&mut body, &mut file).with_context(|| format!("failed to download {url}"))?;
    drop(file);

    let actual = sha256_file(&partial)?;
    if actual != sha256 {
        let _ = fs::remove_file(&partial);
        bail!("checksum mismatch for {url}: expected {sha256}, got {actual}");
    }
    fs::rename(&partial, path)
        .with_context(|| format!("failed to move download to {}", path.display()))?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_as_lowercase_hex() {
        let digest = Sha256::digest(b"abc");
        assert_eq!(
            hex(&digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
