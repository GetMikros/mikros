//! The Secure Boot runs of `cargo xtask test`: the test image, signed
//! with a key made for the run, boots on the Secure Boot firmware with
//! that key enrolled, and the firmware refuses the same image unsigned.
//! The kernel starting shows that the firmware started the boot manager
//! and the boot manager the bootloader.

use super::{key, vars};
use crate::build::{self, Kind};
use crate::qemu::{self, MachineArgs};
use crate::{firmware, image, scenario};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The kernel's first line.
const KERNEL_STARTS: &str = "Mikros kernel";
/// The end of the line that the firmware writes when Secure Boot keeps it
/// from loading a boot option.
const DENIED: &str = ": Access Denied";

fn start(machine: &MachineArgs, dir: &Path, store: &[u8]) -> Result<Child> {
    let firmware = firmware::secure_boot_paths();
    let disk = dir.join("disk.img");
    let vars = disk.with_extension("vars.fd");
    fs::write(&vars, store).with_context(|| format!("writing {}", vars.display()))?;
    let log = dir.join("serial.log");
    let _ = fs::remove_file(&log);
    crate::test::scratch_disks(&log)?;
    let mut machine = machine.clone();
    machine.gpu = qemu::Gpu::Basic;
    let args = crate::test::test_args(
        qemu::args(
            &machine,
            qemu::Processor::available(),
            &firmware,
            &vars,
            &disk,
        ),
        &log,
        false,
    );
    Command::new(qemu::QEMU)
        .args(args)
        .spawn()
        .with_context(|| format!("failed to start {}", qemu::QEMU))
}

fn stop(mut qemu: Child) {
    let _ = qemu.kill();
    let _ = qemu.wait();
}

/// Boots the signed image in `dir` and waits for the kernel to start.
fn boots(machine: &MachineArgs, dir: &Path, store: &[u8], timeout: Duration) -> Result<()> {
    let mut qemu = start(machine, dir, store)?;
    let result = scenario::wait_for_line(
        &dir.join("serial.log"),
        KERNEL_STARTS,
        &mut qemu,
        Instant::now() + timeout,
    );
    stop(qemu);
    result.map(|_| ())
}

/// Boots the unsigned image in `dir` and checks that the firmware refuses
/// to load it and the kernel does not start.
fn refused(machine: &MachineArgs, dir: &Path, store: &[u8], timeout: Duration) -> Result<()> {
    let mut qemu = start(machine, dir, store)?;
    let log = dir.join("serial.log");
    let result = scenario::wait_for_line(&log, DENIED, &mut qemu, Instant::now() + timeout);
    stop(qemu);
    let serial = result?;
    if serial.contains(KERNEL_STARTS) {
        bail!("the kernel started");
    }
    Ok(())
}

/// Runs the signed and the unsigned run of the components `built` of
/// the build kind `kind`, under `dir`.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    let firmware = firmware::secure_boot_paths();
    if !firmware.code.exists() {
        bail!("the Secure Boot firmware is missing; fetch it with `cargo xtask setup`");
    }
    // The key of the run stays out of the checkout and is removed after it.
    let key_dir = std::env::temp_dir().join(format!("mikros-secure-boot-{}", std::process::id()));
    let _ = fs::remove_dir_all(&key_dir);
    let key = key::load_or_create(&key_dir);
    let _ = fs::remove_dir_all(&key_dir);
    let key = key?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let template =
        fs::read(&firmware.vars).with_context(|| format!("reading {}", firmware.vars.display()))?;
    let store = vars::enroll(&template, &key.cert, now)?;

    let signed = dir.join("signed");
    fs::create_dir_all(&signed)?;
    image::create_signed(kind, built, &signed.join("disk.img"), &key)?;
    boots(machine, &signed, &store, timeout).with_context(|| {
        format!(
            "the signed image did not boot with Secure Boot; serial output in {}",
            signed.join("serial.log").display()
        )
    })?;
    println!("the signed image booted with Secure Boot");

    let unsigned = dir.join("unsigned");
    fs::create_dir_all(&unsigned)?;
    image::create(kind, built, &unsigned.join("disk.img"))?;
    refused(machine, &unsigned, &store, timeout).with_context(|| {
        format!(
            "the firmware did not refuse the unsigned image; serial output in {}",
            unsigned.join("serial.log").display()
        )
    })?;
    println!("the firmware refused the unsigned image");
    Ok(())
}
