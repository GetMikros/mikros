//! `cargo xtask build`: builds the system components.

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The build kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Kind {
    Production,
    Debug,
    Fuzz,
}

impl Kind {
    /// The name of the kind on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Production => "production",
            Kind::Debug => "debug",
            Kind::Fuzz => "fuzz",
        }
    }

    /// The Cargo profile used for this kind.
    fn profile(self) -> &'static str {
        match self {
            Kind::Production => "release",
            Kind::Debug | Kind::Fuzz => "dev",
        }
    }

    /// The directory Cargo writes the profile's output to.
    fn profile_dir(self) -> &'static str {
        match self {
            Kind::Production => "release",
            Kind::Debug | Kind::Fuzz => "debug",
        }
    }
}

#[derive(clap::Args)]
pub struct Args {
    /// The build kind.
    #[arg(long, value_enum, default_value_t = Kind::Debug)]
    pub kind: Kind,
}

/// The paths of the built components.
pub struct Built {
    pub bootmenu: PathBuf,
    pub bootloader: PathBuf,
    pub kernel: PathBuf,
    pub boot_image: PathBuf,
}

/// Builds every component of the build kind `kind`.
pub fn build(kind: Kind) -> Result<Built> {
    let bootmenu = cargo_build("bootmenu", "bootmenu", "x86_64-unknown-uefi", kind)?;
    println!("built    {}", bootmenu.display());
    let bootloader = cargo_build("mikrosboot", "mikrosboot", "x86_64-unknown-uefi", kind)?;
    println!("built    {}", bootloader.display());
    if kind == Kind::Fuzz {
        bail!("the fuzz build of the kernel is not implemented yet");
    }
    let kernel = cargo_build("kernel-x86_64", "kernel", KERNEL_TARGET, kind)?;
    println!("built    {}", kernel.display());
    let debug_info = crate::debuginfo::split(&kernel)?;
    println!("built    {}", debug_info.display());
    let init = user_build("init", kind)?;
    println!("built    {}", init.display());
    let boot_image = boot_image(kind, &[("init", &init)])?;
    println!("built    {}", boot_image.display());
    Ok(Built {
        bootmenu,
        bootloader,
        kernel,
        boot_image,
    })
}

/// Writes the boot archive with each of `members`, given by name and the
/// path of its contents, as a regular file.
fn boot_image(kind: Kind, members: &[(&str, &Path)]) -> Result<PathBuf> {
    let dir = crate::workspace_root()
        .join("target/boot")
        .join(kind.name());
    fs::create_dir_all(&dir)?;
    let path = dir.join("boot.img");
    let file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    let mut archive = tar::Builder::new(file);
    for &(name, source) in members {
        let data = fs::read(source).with_context(|| format!("reading {}", source.display()))?;
        let mut header = tar::Header::new_ustar();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        archive
            .append_data(&mut header, name, data.as_slice())
            .with_context(|| format!("adding {name} to the boot archive"))?;
    }
    archive.finish().context("finishing the boot archive")?;
    Ok(path)
}

pub fn run(args: &Args) -> Result<()> {
    build(args.kind).map(drop)
}

/// The target the kernel is built for.
pub const KERNEL_TARGET: &str = "x86_64-unknown-none";

/// The target user-space programs are built for.
const USER_TARGET: &str = "targets/x86_64-unknown-mikros.json";

/// Builds the user-space program `bin`, a package of the `user`
/// workspace of the same name, and returns its path.
fn user_build(bin: &str, kind: Kind) -> Result<PathBuf> {
    let root = crate::workspace_root();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    // The user workspace has its own configuration, which Cargo reads
    // from the directory it runs in.
    let status = Command::new(cargo)
        .current_dir(root.join("user"))
        .args(["build", "--package", bin, "--bin", bin, "--target"])
        .arg(root.join(USER_TARGET))
        .args(["--profile", kind.profile()])
        .status()
        .context("failed to run cargo")?;
    if !status.success() {
        bail!("building {bin} failed");
    }
    Ok(root
        .join("user/target/x86_64-unknown-mikros")
        .join(kind.profile_dir())
        .join(bin))
}

/// Builds the binary `bin` of `package` for `target` and returns its path.
fn cargo_build(package: &str, bin: &str, target: &str, kind: Kind) -> Result<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .current_dir(crate::workspace_root())
        .args([
            "build",
            "--package",
            package,
            "--bin",
            bin,
            "--target",
            target,
        ])
        .args(["--profile", kind.profile()])
        .status()
        .context("failed to run cargo")?;
    if !status.success() {
        bail!("building {package} failed");
    }
    let extension = if target.ends_with("-uefi") { "efi" } else { "" };
    let mut path = crate::workspace_root()
        .join("target")
        .join(target)
        .join(kind.profile_dir())
        .join(bin);
    path.set_extension(extension);
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_boot_archive() {
        let dir = crate::workspace_root().join("target/xtask-test");
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("init");
        fs::write(&source, b"\x7fELF test").unwrap();
        let path = boot_image(Kind::Fuzz, &[("init", &source)]).unwrap();
        assert!(path.ends_with("target/boot/fuzz/boot.img"));
        let bytes = fs::read(&path).unwrap();
        let mut archive = tar::Archive::new(bytes.as_slice());
        let mut entries = archive.entries().unwrap();
        let mut init = entries.next().unwrap().unwrap();
        assert_eq!(init.path().unwrap().to_str(), Some("init"));
        assert_eq!(init.header().entry_type(), tar::EntryType::Regular);
        assert_eq!(&init.header().as_bytes()[257..263], b"ustar\0");
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut init, &mut data).unwrap();
        assert_eq!(data, b"\x7fELF test");
        assert!(entries.next().is_none());
    }
}
