//! `cargo xtask build`: builds the system components.

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use std::path::PathBuf;
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

/// The paths of the built components. Components that are not built yet
/// are `None`.
pub struct Built {
    pub bootmenu: PathBuf,
    pub bootloader: Option<PathBuf>,
    pub kernel: Option<PathBuf>,
    pub boot_image: Option<PathBuf>,
}

/// Builds every component of the build kind `kind`.
pub fn build(kind: Kind) -> Result<Built> {
    let bootmenu = cargo_build("bootmenu", "x86_64-unknown-uefi", kind)?;
    println!("built    {}", bootmenu.display());
    Ok(Built {
        bootmenu,
        bootloader: None,
        kernel: None,
        boot_image: None,
    })
}

pub fn run(args: &Args) -> Result<()> {
    build(args.kind).map(drop)
}

/// Builds the binary of `package` for `target` and returns its path.
fn cargo_build(package: &str, target: &str, kind: Kind) -> Result<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .current_dir(crate::workspace_root())
        .args(["build", "--package", package, "--target", target])
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
        .join(package);
    path.set_extension(extension);
    Ok(path)
}
