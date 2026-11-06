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
    /// Expose the display over VNC, for example 0.0.0.0:0 (port 5900).
    #[arg(long, value_name = "ADDRESS:DISPLAY")]
    pub vnc: Option<String>,
    #[command(flatten)]
    pub machine: MachineArgs,
}

pub fn run(args: &Args) -> Result<()> {
    if args.vnc.is_some() && args.machine.gpu == qemu::Gpu::Venus {
        bail!("VNC is not supported with --gpu venus; use --gpu virtio or --gpu basic");
    }
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

    let processor = qemu::Processor::available();
    if processor != qemu::Processor::Host {
        println!("KVM is not available; using QEMU's emulation");
    }
    let mut command = Command::new(qemu::QEMU);
    command.args(qemu::args(
        &args.machine,
        processor,
        &firmware,
        &vars,
        &disk,
    ));
    if let Some(address) = &args.vnc {
        command.args(["-display", &format!("vnc={address}")]);
    }
    let status = command
        .status()
        .with_context(|| format!("failed to start {}", qemu::QEMU))?;
    if !status.success() {
        bail!("QEMU exited with {status}");
    }
    Ok(())
}
