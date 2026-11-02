//! `cargo xtask test`: runs the host unit tests and the integration
//! tests in QEMU.

use crate::build::{self, BootImage, Kind};
use crate::qemu::{self, MachineArgs};
use crate::{firmware, image};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(clap::Args)]
pub struct Args {
    /// The build kind to test.
    #[arg(long, value_enum, default_value_t = Kind::Debug)]
    pub kind: Kind,
    /// Seconds an integration test may run before it fails.
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,
    /// Skip the host unit tests.
    #[arg(long)]
    pub no_host: bool,
    #[command(flatten)]
    pub machine: MachineArgs,
}

/// The value the test init writes to `isa-debug-exit` when every test
/// passed; QEMU exits with twice the value plus one.
const PASSED: i32 = 0x10 * 2 + 1;

/// The line that marks a kernel panic in the serial output.
const PANIC_LINE: &str = "KERNEL PANIC:";

/// Where the disk image and the serial output of a test run go.
fn test_dir(kind: Kind) -> PathBuf {
    crate::workspace_root()
        .join("target/test")
        .join(kind.name())
}

/// Whether a run passed, given QEMU's exit code and the serial output,
/// or why it failed.
fn verdict(code: Option<i32>, serial: &str) -> Result<(), String> {
    if serial.lines().any(|l| l.contains(PANIC_LINE)) {
        return Err("the kernel panicked".into());
    }
    match code {
        Some(PASSED) => Ok(()),
        Some(code) => Err(format!("QEMU exited with {code}")),
        None => Err("QEMU was stopped by a signal".into()),
    }
}

/// The arguments of `qemu::args` with the serial port written to `log`,
/// no display, the debug exit device, and no reboot.
fn test_args(mut args: Vec<String>, log: &Path) -> Vec<String> {
    if let Some(i) = args.iter().position(|a| a == "-serial") {
        args[i + 1] = format!("file:{}", log.display());
    }
    args.extend(
        [
            "-display",
            "none",
            "-device",
            "isa-debug-exit,iobase=0xf4,iosize=0x04",
            "-no-reboot",
        ]
        .map(String::from),
    );
    args
}

/// Boots the test image and returns QEMU's exit code, or fails when the
/// run takes longer than `timeout`.
fn boot(args: &Args, disk: &Path, log: &Path) -> Result<Option<i32>> {
    let firmware = firmware::paths();
    if !firmware.code.exists() {
        bail!("the firmware is missing; fetch it with `cargo xtask setup`");
    }
    let vars = disk.with_extension("vars.fd");
    fs::copy(&firmware.vars, &vars)
        .with_context(|| format!("copying the firmware variables to {}", vars.display()))?;
    let kvm = qemu::kvm_available();
    let qemu_args = test_args(qemu::args(&args.machine, kvm, &firmware, &vars, disk), log);
    let mut child = Command::new(qemu::QEMU)
        .args(qemu_args)
        .spawn()
        .with_context(|| format!("failed to start {}", qemu::QEMU))?;
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code());
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("the integration tests timed out after {} s", args.timeout);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn run(args: &Args) -> Result<()> {
    if !args.no_host {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let status = Command::new(cargo)
            .current_dir(crate::workspace_root())
            .args(["test", "--workspace"])
            .status()
            .context("failed to run cargo")?;
        if !status.success() {
            bail!("the host unit tests failed");
        }
    }

    let built = build::build_for(args.kind, BootImage::Test)?;
    let dir = test_dir(args.kind);
    fs::create_dir_all(&dir)?;
    let disk = dir.join("disk.img");
    image::create(args.kind, &built, &disk)?;
    let log = dir.join("serial.log");
    let code = boot(args, &disk, &log)?;
    let serial = fs::read(&log).with_context(|| format!("reading {}", log.display()))?;
    let serial = String::from_utf8_lossy(&serial);
    for line in serial.lines().filter(|l| l.contains("TEST")) {
        println!("{line}");
    }
    match verdict(code, &serial) {
        Ok(()) => {
            println!(
                "integration tests passed; serial output in {}",
                log.display()
            );
            Ok(())
        }
        Err(why) => bail!(
            "integration tests failed: {why}; serial output in {}",
            log.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_only_on_the_exit_code_without_a_panic() {
        assert_eq!(verdict(Some(PASSED), "TEST a PASSED\n"), Ok(()));
        assert!(verdict(Some(35), "").is_err());
        assert!(verdict(None, "").is_err());
        assert!(verdict(Some(PASSED), "x\nKERNEL PANIC: oops\n").is_err());
    }

    #[test]
    fn writes_serial_output_to_the_log() {
        let args = vec!["-m".into(), "1G".into(), "-serial".into(), "stdio".into()];
        let args = test_args(args, Path::new("/t/serial.log"));
        assert_eq!(args[3], "file:/t/serial.log");
        assert!(args.contains(&"isa-debug-exit,iobase=0xf4,iosize=0x04".to_string()));
        assert!(args.contains(&"-no-reboot".to_string()));
    }
}
