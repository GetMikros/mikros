//! Development tasks for the Mikros repository, run with `cargo xtask`.

mod build;
mod check;
mod firmware;
mod image;
mod qemu;
mod run;
mod setup;
mod tools;

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "cargo xtask", bin_name = "cargo xtask")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build the system components.
    Build(build::Args),
    /// Check formatting, lints and the commits since the base branch.
    Check(check::Args),
    /// Create a bootable disk image.
    Image(image::Args),
    /// Boot the disk image in QEMU.
    Run(run::Args),
    /// Check the host tools and fetch the pinned firmware.
    Setup(setup::Args),
}

/// The root of the repository, which holds the Cargo workspace.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is inside the workspace")
        .to_owned()
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Build(args) => build::run(&args),
        Command::Check(args) => check::run(&args),
        Command::Image(args) => image::run(&args),
        Command::Run(args) => run::run(&args),
        Command::Setup(args) => setup::run(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
