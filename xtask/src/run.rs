//! `cargo xtask run`: boots the disk image in QEMU.

use crate::build::Kind;
use crate::qemu::{self, MachineArgs};
use crate::{firmware, image};
use anyhow::{Context, Result, bail};
use std::fs;
use std::process::Command;

#[derive(clap::Args)]
pub struct Args {
    /// The build kind whose image to boot.
    #[arg(long, value_enum, default_value_t = Kind::Debug)]
    pub kind: Kind,
    /// Boot the live image instead.
    #[arg(long)]
    pub live: bool,
    #[command(flatten)]
    pub machine: MachineArgs,
}

pub fn run(args: &Args) -> Result<()> {
    let disk = image::path(args.kind, args.live);
    if !disk.exists() {
        bail!(
            "{} does not exist; create it with `cargo xtask image`",
            disk.display()
        );
    }
    let firmware = firmware::paths();
    if !firmware.code.exists() {
        bail!("the firmware is missing; fetch it with `cargo xtask setup`");
    }
    // Each image keeps its own firmware variables, starting from the
    // pinned defaults.
    let vars = disk.with_extension("vars.fd");
    if !vars.exists() {
        fs::copy(&firmware.vars, &vars)
            .with_context(|| format!("copying the firmware variables to {}", vars.display()))?;
    }

    let kvm = qemu::kvm_available();
    if !kvm {
        println!("KVM is not available; using QEMU's emulation");
    }
    let status = Command::new(qemu::QEMU)
        .args(qemu::args(&args.machine, kvm, &firmware, &vars, &disk))
        .status()
        .with_context(|| format!("failed to start {}", qemu::QEMU))?;
    if !status.success() {
        bail!("QEMU exited with {status}");
    }
    Ok(())
}
