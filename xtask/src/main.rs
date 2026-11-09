//! Development tasks for the Mikros repository, run with `cargo xtask`.

mod acpica;
mod boot_menu;
mod btrfs;
mod build;
mod build_systems;
mod casefold;
mod check;
mod cmake;
mod debug;
mod debug_build;
mod debuginfo;
mod entry_state;
mod extended;
mod fetch;
mod firmware;
mod fuzz;
mod image;
mod llvm;
mod loader_check;
mod machine_state;
mod meson;
mod musl;
mod port;
mod port_tests;
mod ports;
mod power;
mod qemu;
mod run;
mod rust_std;
mod scenario;
mod setup;
mod symbolize;
mod sysroot;
mod test;
mod tools;
mod tzdata;
mod volumes;

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
    /// Build ACPICA, which the ACPI server runs on, into a static library.
    Acpica(acpica::Args),
    /// Build Linux's btrfs code on the btrfs shim into a static library.
    Btrfs(btrfs::Args),
    /// Build the system components.
    Build(build::Args),
    /// Generate the case folding table of mikros-casefold.
    Casefold(casefold::Args),
    /// Check formatting, lints and the commits since the base branch.
    Check(check::Args),
    /// Boot the disk image in QEMU halted and attach GDB to the kernel.
    Debug(debug::Args),
    /// Run a host fuzz target.
    Fuzz(fuzz::Args),
    /// Create a bootable disk image.
    Image(image::Args),
    /// Boot the disk image in QEMU.
    Run(run::Args),
    /// Check the host tools and fetch the pinned firmware.
    Setup(setup::Args),
    /// Replace the backtrace addresses in the panic reports of a serial
    /// log with their functions, files and lines.
    Symbolize(symbolize::Args),
    /// Build the sysroot that C and C++ programs are built against.
    Sysroot(sysroot::Args),
    /// Run the host unit tests and the integration tests in QEMU.
    Test(test::Args),
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
        Command::Acpica(args) => acpica::run(&args),
        Command::Btrfs(args) => btrfs::run(&args),
        Command::Build(args) => build::run(&args),
        Command::Casefold(args) => casefold::run(&args),
        Command::Check(args) => check::run(&args),
        Command::Debug(args) => debug::run(&args),
        Command::Fuzz(args) => fuzz::run(&args),
        Command::Image(args) => image::run(&args),
        Command::Run(args) => run::run(&args),
        Command::Setup(args) => setup::run(&args),
        Command::Symbolize(args) => symbolize::run(&args),
        Command::Sysroot(args) => sysroot::run(&args),
        Command::Test(args) => test::run(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
