//! `cargo xtask test`: runs the host unit tests, replays the host fuzz
//! corpora and runs the integration tests in QEMU. With `--host`, it runs
//! only the host unit tests and the replays.

use crate::build::{self, BootImage, Kind};
use crate::qemu::{self, MachineArgs};
use crate::{firmware, image};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(clap::Args, Clone)]
pub struct Args {
    /// The build kind to test.
    #[arg(long, value_enum, default_value_t = Kind::Debug)]
    pub kind: Kind,
    /// Seconds an integration test may run before it fails.
    #[arg(long, default_value_t = 240)]
    pub timeout: u64,
    /// Skip the host unit tests and the replay of the fuzz corpora.
    #[arg(long)]
    pub no_host: bool,
    /// Run only the host unit tests and the replay of the fuzz corpora,
    /// without building the test image or starting QEMU.
    #[arg(long, conflicts_with = "no_host")]
    pub host: bool,
    /// Run the pass with kernel page-table isolation on QEMU's emulated
    /// Intel processor even when the host's could be used under KVM. The
    /// emulated processor is about ten times slower, which shows races
    /// and slow paths that KVM hides.
    #[arg(long, conflicts_with = "host")]
    pub emulated_isolation: bool,
    #[command(flatten)]
    pub machine: MachineArgs,
}

/// The memory of the test machines unless `--memory` says otherwise:
/// the test boot image holds the images of all the tests' systems.
const TEST_MEMORY: &str = "3G";

/// The value the test init writes to `isa-debug-exit` when every test
/// passed; QEMU exits with twice the value plus one.
pub(crate) const PASSED: i32 = 0x10 * 2 + 1;

/// The line that marks a kernel panic in the serial output.
const PANIC_LINE: &str = "KERNEL PANIC:";

/// The kernel functions that dispatch interrupts and handle the timer
/// interrupt, by their demangled names. The fuzz build's coverage must
/// record none of their code while it handles a hardware interrupt.
const INTERRUPT_FUNCTIONS: [&str; 6] = [
    "kernel::idt::interrupt",
    "kernel::timer::interrupt",
    "kernel_core::sched::timer_interrupt",
    "kernel_core::sched::maintain",
    "kernel::time::maintain",
    "<kernel_core::ktimer::Armed>::fire",
];

/// Runs the system tests in a test image as the kernel's tests run: on
/// the machine of `args`, on QEMU's AMD IOMMU, and with kernel page-table
/// isolation. They run only under KVM and not in fuzz builds: selftest
/// gives each test 60 seconds whatever the machine, which an emulated
/// processor is too slow for, and the fuzz build's kernel does not yet
/// boot the system that runs them.
fn system_tests(args: &Args, dir: &Path) -> Result<()> {
    let processor = qemu::Processor::available();
    if args.kind == Kind::Fuzz || processor != qemu::Processor::Host {
        println!("warning  the system tests run only under KVM and not in fuzz builds");
        return Ok(());
    }
    let timeout = Duration::from_secs(args.timeout);
    let passed = crate::system_tests::run(
        &args.machine,
        processor,
        args.kind,
        &dir.join("system"),
        timeout,
    )?;
    println!("system tests passed: {passed}");
    if !args.machine.no_iommu {
        let mut amd = args.machine.clone();
        amd.amd_iommu = true;
        let dir = dir.join("system-amd-iommu");
        let passed = crate::system_tests::run(&amd, processor, args.kind, &dir, timeout)?;
        println!("system tests passed on AMD-Vi: {passed}");
    }
    let isolating = qemu::Processor::isolating();
    if args.emulated_isolation || isolating != qemu::Processor::HostWithoutRdclNo {
        println!("warning  the system tests run with page-table isolation only under KVM");
        return Ok(());
    }
    let dir = dir.join("system-isolation");
    let passed = crate::system_tests::run(&args.machine, isolating, args.kind, &dir, timeout)?;
    println!("system tests passed with kernel page-table isolation: {passed}");
    Ok(())
}

/// The kernel function that only the idle loop runs. The fuzz build's
/// coverage must record none of its code, as the idle loop runs on behalf
/// of no thread.
const IDLE_FUNCTIONS: [&str; 1] = ["kernel_core::sched::halt_until_interrupt"];

/// The address ranges, start and end, of the functions `names` and
/// their closures in `symbols`, which `llvm-nm -S -C` printed, or the
/// first function it lacks.
fn code_ranges(symbols: &str, names: &[&'static str]) -> Result<Vec<(u64, u64)>, &'static str> {
    let mut ranges = Vec::new();
    for &name in names {
        let closure = format!("{name}::{{closure");
        let found = ranges.len();
        for line in symbols.lines() {
            let mut fields = line.splitn(4, ' ');
            let (Some(value), Some(size), Some(_), Some(symbol)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if symbol != name && !symbol.starts_with(&closure) {
                continue;
            }
            let hex = |text: &str| u64::from_str_radix(text, 16).ok();
            if let (Some(start), Some(size)) = (hex(value), hex(size)) {
                ranges.push((start, start + size));
            }
        }
        if ranges.len() == found {
            return Err(name);
        }
    }
    Ok(ranges)
}

/// The kernel command line argument that gives the test init `ranges`:
/// `key=` and each range as `start-end` in hexadecimal, separated by
/// commas.
fn code_arg(key: &str, ranges: &[(u64, u64)]) -> String {
    let list: Vec<String> = ranges.iter().map(|(s, e)| format!("{s:x}-{e:x}")).collect();
    format!("{key}={}", list.join(","))
}

/// The command line word that has the kernel call one of the firmware's
/// runtime services while it starts, and the line it then logs when the
/// call succeeds.
const UEFI_SELFTEST: &str = "uefi_selftest";
const UEFI_SELFTEST_PASSED: &str = "uefi: GetNextVariableName returned 0x0";

/// The command line word that has the test init leave out the test of
/// ported software, whose suites take minutes when emulated. Only the
/// first run of a build kind other than fuzz runs it.
const SKIP_PORTED: &str = "skip=ported_software_passes_its_tests";

/// What the integration tests of `kind` add to the kernel command line:
/// for the fuzz build, the interrupt code and the idle code of `kernel`,
/// and `SKIP_PORTED`.
fn test_cmdline(kind: Kind, kernel: &Path) -> Result<String> {
    if kind != Kind::Fuzz {
        return Ok(String::new());
    }
    let symbols = Command::new(crate::debuginfo::llvm_tool("llvm-nm")?)
        .args(["-S", "-C", "--defined-only"])
        .arg(kernel)
        .output()
        .context("failed to run llvm-nm")?;
    let symbols = String::from_utf8_lossy(&symbols.stdout);
    let mut args = Vec::new();
    for (key, names) in [
        ("interrupt_code", &INTERRUPT_FUNCTIONS[..]),
        ("idle_code", &IDLE_FUNCTIONS[..]),
    ] {
        match code_ranges(&symbols, names) {
            Ok(ranges) => args.push(code_arg(key, &ranges)),
            Err(name) => bail!("{} has no function {name}", kernel.display()),
        }
    }
    args.push(SKIP_PORTED.to_owned());
    Ok(args.join(" "))
}

/// Where the disk image and the serial output of a test run go.
fn test_dir(kind: Kind) -> PathBuf {
    crate::workspace_root()
        .join("target/test")
        .join(kind.name())
}

/// Whether a run passed, given QEMU's exit code and the serial output,
/// or why it failed.
pub(crate) fn verdict(code: Option<i32>, serial: &str) -> Result<(), String> {
    if serial.lines().any(|l| l.contains(PANIC_LINE)) {
        return Err("the kernel panicked".into());
    }
    match code {
        Some(PASSED) => Ok(()),
        Some(code) => Err(format!("QEMU exited with {code}")),
        None => Err("QEMU was stopped by a signal".into()),
    }
}

/// The scratch disks of the test machine, in the directory of its serial
/// log: one behind an NVMe controller, one on port 2 of the q35
/// machine's AHCI controller, one USB mass storage device on the xHCI
/// controller, and, in the durability run only, one behind a virtio-blk
/// controller, each with the GUIDs of its table and of its one
/// partition.
const SCRATCH_DISKS: [(&str, &str, &str); 4] = [
    (
        "nvme.img",
        "4e564d65-0000-4000-8000-0000000000d1",
        "4e564d65-0000-4000-8000-000000000001",
    ),
    (
        "ahci.img",
        "41484349-0000-4000-8000-0000000000d2",
        "41484349-0000-4000-8000-000000000002",
    ),
    (
        "usb.img",
        "55534221-0000-4000-8000-0000000000d3",
        "55534221-0000-4000-8000-000000000003",
    ),
    (
        "virtio.img",
        "56495254-0000-4000-8000-0000000000d4",
        "56495254-0000-4000-8000-000000000004",
    ),
];
/// The size of a scratch disk, in sectors.
const SCRATCH_SECTORS: u64 = 2048;
/// The type of a scratch disk's partition: Linux file system data.
const SCRATCH_TYPE: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";

/// The first block of a scratch disk's partition.
pub(crate) const SCRATCH_FIRST_LBA: u64 = 34;

/// The file, beside the scratch disk `name`, of QEMU's log of the writes
/// and flushes that reach it when the disk is logged.
pub(crate) fn writes_log(name: &str) -> String {
    format!("{}.writes", name.trim_end_matches(".img"))
}

/// Creates the scratch disks next to `log`, and an empty log of the
/// writes of each. Each is a GPT disk of 1 MiB whose one partition, named
/// `scratch`, spans every usable block, LBA 34 to 2014, and holds zeros.
pub fn scratch_disks(log: &Path) -> Result<()> {
    use crate::image::gpt;
    let dir = log.parent().unwrap_or(Path::new("."));
    for (name, disk_guid, partition_guid) in SCRATCH_DISKS {
        let mut image = std::io::Cursor::new(vec![0u8; (SCRATCH_SECTORS * gpt::SECTOR) as usize]);
        let partition = gpt::Partition {
            type_guid: uuid::Uuid::parse_str(SCRATCH_TYPE)?,
            guid: uuid::Uuid::parse_str(partition_guid)?,
            name: "scratch",
            first_lba: SCRATCH_FIRST_LBA,
            last_lba: SCRATCH_SECTORS - 34,
        };
        let disk_guid = uuid::Uuid::parse_str(disk_guid)?;
        gpt::write(&mut image, SCRATCH_SECTORS, disk_guid, &[partition])?;
        let path = dir.join(name);
        fs::write(&path, image.into_inner())
            .with_context(|| format!("writing {}", path.display()))?;
        let writes = dir.join(writes_log(name));
        fs::write(&writes, []).with_context(|| format!("writing {}", writes.display()))?;
    }
    Ok(())
}

/// The arguments of `qemu::args` with the serial port written to `log`,
/// no display, the debug exit device, QEMU's `edu` device for DMA tests,
/// able to reach the first 4 GiB, an xHCI controller at 00:1d.0, the
/// scratch disks of `scratch_disks` with the serial number `mikros`, the
/// last as a USB mass storage device on port 1 of that controller, a USB
/// hub on its port 2 with a USB keyboard on the hub's port 1, QEMU's
/// i6300esb watchdog, whose memory BAR is 16 bytes long, and no
/// reboot. The devices follow
/// the IOMMU, which must come first.
///
/// With `logged`, as the durability run has it, QEMU logs every write and
/// flush that reaches a scratch disk in the disk's `writes_log`, with its
/// `blklogwrites` block driver, and the machine has the virtio-blk scratch
/// disk too, after the other devices.
pub fn test_args(mut args: Vec<String>, log: &Path, logged: bool) -> Vec<String> {
    if let Some(i) = args.iter().position(|a| a == "-serial") {
        args[i + 1] = format!("file:{}", log.display());
    }
    let dir = log.parent().unwrap_or(Path::new("."));
    let drive = |id: &str, name: &str| -> Vec<String> {
        let path = dir.join(name);
        if !logged {
            let drive = format!("if=none,id={id},format=raw,file={}", path.display());
            return vec!["-drive".into(), drive];
        }
        let writes = dir.join(writes_log(name));
        vec![
            "-blockdev".into(),
            format!(
                "driver=file,node-name={id}-file,filename={}",
                path.display()
            ),
            "-blockdev".into(),
            format!(
                "driver=file,node-name={id}-log,filename={}",
                writes.display()
            ),
            "-blockdev".into(),
            format!(
                "driver=blklogwrites,node-name={id},file={id}-file,log={id}-log,log-sector-size=512"
            ),
        ]
    };
    let device = |device: &str| ["-device".to_owned(), device.to_owned()];
    args.extend(["-display", "none"].map(String::from));
    args.extend(device("isa-debug-exit,iobase=0xf4,iosize=0x04"));
    args.extend(device("edu,dma_mask=0xffffffff"));
    args.extend(drive("scratch-nvme", SCRATCH_DISKS[0].0));
    args.extend(device("nvme,drive=scratch-nvme,serial=mikros"));
    args.extend(drive("scratch-ahci", SCRATCH_DISKS[1].0));
    args.extend(device("ide-hd,drive=scratch-ahci,bus=ide.2,serial=mikros"));
    args.extend(device("qemu-xhci,id=usb,addr=0x1d"));
    args.extend(drive("scratch-usb", SCRATCH_DISKS[2].0));
    args.extend(device(
        "usb-storage,id=stick,bus=usb.0,port=1,drive=scratch-usb,serial=mikros",
    ));
    args.extend(device("usb-hub,bus=usb.0,port=2"));
    args.extend(device("usb-kbd,id=usbkbd,bus=usb.0,port=2.1"));
    // A watchdog whose memory BAR is smaller than a page.
    args.extend(device("i6300esb"));
    if logged {
        args.extend(drive("scratch-virtio", SCRATCH_DISKS[3].0));
        args.extend(device(
            "virtio-blk-pci,drive=scratch-virtio,disable-legacy=on,serial=mikros",
        ));
    }
    args.push("-no-reboot".into());
    args
}

/// How many of the last serial lines a timed-out run prints.
const TAIL_LINES: usize = 40;

/// What a timed-out run prints of its serial output: the `TEST` lines,
/// as a finished run prints them, then the last `TAIL_LINES` lines, which
/// show where the run stopped.
fn timeout_report(serial: &str) -> String {
    let mut report = String::new();
    for line in serial.lines().filter(|l| l.contains("TEST")) {
        report.push_str(line);
        report.push('\n');
    }
    let lines: Vec<&str> = serial.lines().collect();
    let tail = &lines[lines.len().saturating_sub(TAIL_LINES)..];
    report.push_str(&format!("last {} serial lines:\n", tail.len()));
    for line in tail {
        report.push_str(line);
        report.push('\n');
    }
    report
}

/// Boots the test image `disk` on `processor` and returns QEMU's exit
/// code, or fails when the run takes longer than `timeout` seconds.
fn boot(
    args: &Args,
    processor: qemu::Processor,
    timeout: u64,
    disk: &Path,
    log: &Path,
) -> Result<Option<i32>> {
    let firmware = firmware::paths();
    if !firmware.code.exists() {
        bail!("the firmware is missing; fetch it with `cargo xtask setup`");
    }
    let vars = disk.with_extension("vars.fd");
    fs::copy(&firmware.vars, &vars)
        .with_context(|| format!("copying the firmware variables to {}", vars.display()))?;
    scratch_disks(log)?;
    let qemu_args = test_args(
        qemu::args(&args.machine, processor, &firmware, &vars, disk),
        log,
        false,
    );
    let mut child = Command::new(qemu::QEMU)
        .args(qemu_args)
        .spawn()
        .with_context(|| format!("failed to start {}", qemu::QEMU))?;
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code());
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            let serial = fs::read(log).unwrap_or_default();
            print!("{}", timeout_report(&String::from_utf8_lossy(&serial)));
            bail!(
                "the integration tests timed out after {timeout} s; serial output in {}",
                log.display()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Runs the integration tests of `disk` on `processor`, prints their
/// results and fails unless they passed and the kernel logged `expected`.
/// `what` tells the run apart in the messages.
fn integration(
    args: &Args,
    processor: qemu::Processor,
    timeout: u64,
    disk: &Path,
    log: &Path,
    what: &str,
    expected: &str,
) -> Result<()> {
    let code = boot(args, processor, timeout, disk, log)?;
    let serial = fs::read(log).with_context(|| format!("reading {}", log.display()))?;
    let serial = String::from_utf8_lossy(&serial);
    for line in serial.lines().filter(|l| l.contains("TEST")) {
        println!("{line}");
    }
    if let Err(why) = verdict(code, &serial) {
        // What the tests logged as errors besides their results tells why
        // one failed.
        for line in serial
            .lines()
            .filter(|l| l.contains("] E ") && !l.contains("TEST"))
        {
            println!("{line}");
        }
        bail!(
            "integration tests{what} failed: {why}; serial output in {}",
            log.display()
        );
    }
    if !serial.contains(expected) {
        bail!(
            "integration tests{what} ran without the kernel logging \"{expected}\"; serial output in {}",
            log.display()
        );
    }
    println!(
        "integration tests{what} passed; serial output in {}",
        log.display()
    );
    Ok(())
}

pub fn run(args: &Args) -> Result<()> {
    let mut args = args.clone();
    args.machine
        .memory
        .get_or_insert_with(|| TEST_MEMORY.to_owned());
    let args = &args;
    if !args.no_host {
        // The btrfs crate's tests link the host's library of the shim.
        crate::btrfs::build(crate::btrfs::Flavor::Host)?;
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        // The root workspace, and the user workspace's packages with
        // tests that run on the host, its default members.
        for (dir, args) in [("", &["test", "--workspace"][..]), ("user", &["test"])] {
            let status = Command::new(&cargo)
                .current_dir(crate::workspace_root().join(dir))
                .args(args)
                .status()
                .context("failed to run cargo")?;
            if !status.success() {
                bail!("the host unit tests failed");
            }
        }
        crate::fuzz::replay_corpora()?;
    }
    if args.host {
        return Ok(());
    }

    let built = build::build_for(args.kind, BootImage::Test)?;
    crate::port_tests::run_on_host(&crate::ports::load("xkbcommon")?, &built.xkbcommon)?;
    let dir = test_dir(args.kind);
    fs::create_dir_all(&dir)?;
    let disk = dir.join("disk.img");
    let cmdline = [
        test_cmdline(args.kind, &built.kernel)?.as_str(),
        UEFI_SELFTEST,
    ]
    .join(" ");
    let cmdline = cmdline.trim().to_owned();
    image::create_for_tests(args.kind, &built, &disk, &cmdline)?;
    let log = dir.join("serial.log");
    let processor = qemu::Processor::available();
    integration(
        args,
        processor,
        args.timeout,
        &disk,
        &log,
        "",
        UEFI_SELFTEST_PASSED,
    )?;
    // The kernel's tests once more on QEMU's AMD IOMMU, without the suites
    // of ported projects, which do not depend on the IOMMU. Emulated, its
    // DMA translation is about three times slower than the Intel one's.
    if !args.machine.no_iommu {
        let mut amd = args.clone();
        amd.machine.amd_iommu = true;
        let amd_disk = dir.join("amd-iommu-disk.img");
        let amd_cmdline = [cmdline.as_str(), SKIP_PORTED].join(" ");
        image::create_for_tests(args.kind, &built, &amd_disk, amd_cmdline.trim())?;
        let timeout = match processor {
            qemu::Processor::Host => args.timeout,
            _ => args.timeout * 3,
        };
        let amd_log = dir.join("amd-iommu-serial.log");
        integration(
            &amd,
            processor,
            timeout,
            &amd_disk,
            &amd_log,
            " on AMD-Vi",
            "AMD-Vi units",
        )?;
    }
    // Kernel page-table isolation applies only to Intel processors that do
    // not report RDCL_NO: the host's under KVM with the bit hidden, or
    // QEMU's emulated processor when it names that vendor. Emulation takes
    // about five times as long as KVM.
    if args.kind != Kind::Fuzz {
        let isolation_disk = dir.join("isolation-disk.img");
        let isolation_cmdline = [cmdline.as_str(), SKIP_PORTED].join(" ");
        let isolation_cmdline = isolation_cmdline.trim();
        image::create_for_tests(args.kind, &built, &isolation_disk, isolation_cmdline)?;
        let isolating = if args.emulated_isolation {
            qemu::Processor::EmulatedIntel
        } else {
            qemu::Processor::isolating()
        };
        let timeout = match (processor, isolating) {
            (qemu::Processor::Host, qemu::Processor::EmulatedIntel) => args.timeout * 5,
            _ => args.timeout,
        };
        let isolation_log = dir.join("isolation-serial.log");
        integration(
            args,
            isolating,
            timeout,
            &isolation_disk,
            &isolation_log,
            " with kernel page-table isolation",
            "page-table isolation on",
        )?;
    }
    // The debugger checks boot as cargo xtask debug does, at the kernel's
    // linked addresses.
    let gdb_disk = dir.join("gdb-disk.img");
    let gdb_cmdline = [cmdline.as_str(), image::KASLR_OFF].join(" ");
    image::create_for_tests(args.kind, &built, &gdb_disk, gdb_cmdline.trim())?;
    let gdb_log = dir.join("gdb-serial.log");
    let timeout = Duration::from_secs(args.timeout);
    crate::debug::check_scripts(&args.machine, &built.kernel, &gdb_disk, &gdb_log, timeout)?;
    println!("kernel debugger scripts passed");
    let entry_log = dir.join("entry-serial.log");
    crate::entry_state::check(&args.machine, &built.kernel, &gdb_disk, &entry_log, timeout)?;
    println!("kernel entry state checked");
    crate::machine_state::run(
        &args.machine,
        args.kind,
        &built,
        &dir.join("machine"),
        timeout,
    )?;
    println!("machine state checked");
    let scenarios = dir.join("scenarios");
    crate::scenario::run(&args.machine, args.kind, &built, &scenarios, timeout)?;
    println!("panic scenarios passed");
    let boot_menu = dir.join("boot-menu");
    crate::boot_menu::run(&args.machine, args.kind, &built, &boot_menu, timeout)?;
    println!("boot menu runs passed");
    crate::secure_boot::check::run(
        &args.machine,
        args.kind,
        &built,
        &dir.join("secure-boot"),
        timeout,
    )?;
    println!("Secure Boot runs passed");
    crate::power::run(
        &args.machine,
        args.kind,
        &built,
        &dir.join("power"),
        timeout,
    )?;
    println!("power-off run passed");
    system_tests(args, &dir)?;
    crate::volumes::run(
        &args.machine,
        args.kind,
        &built,
        &dir.join("volumes"),
        timeout,
    )?;
    println!("USB volumes run passed");
    crate::root_disk::run(
        &args.machine,
        args.kind,
        &built,
        &dir.join("root-disk"),
        timeout,
    )?;
    println!("root disk runs passed");
    crate::root_disk::run_bus_restart(
        &args.machine,
        args.kind,
        &built,
        &dir.join("bus-restart"),
        timeout,
    )?;
    println!("bus restart runs passed");
    crate::root_disk::run_acpi_restart(
        &args.machine,
        args.kind,
        &built,
        &dir.join("acpi-restart"),
        timeout,
    )?;
    println!("ACPI restart run passed");
    crate::input::run(
        &args.machine,
        args.kind,
        &built,
        &dir.join("input"),
        timeout,
    )?;
    println!("input run passed");
    crate::console::run(
        &args.machine,
        args.kind,
        &built,
        &dir.join("console"),
        timeout,
    )?;
    println!("console run passed");
    crate::console::run_setup(
        &args.machine,
        args.kind,
        &built,
        &dir.join("setup"),
        timeout,
    )?;
    println!("setup run passed");
    crate::takeover::run(
        &args.machine,
        args.kind,
        &built,
        &dir.join("takeover"),
        timeout,
    )?;
    println!("takeover run passed");
    crate::durability::run(
        &args.machine,
        args.kind,
        &built,
        &dir.join("durability"),
        timeout,
    )?;
    println!("durability run passed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// The arguments of `cargo xtask test`, parsed on their own.
    #[derive(Parser)]
    struct TestCommand {
        #[command(flatten)]
        args: Args,
    }

    #[test]
    fn takes_host_only_without_no_host() {
        let parsed = TestCommand::try_parse_from(["test", "--host"]).unwrap();
        assert!(parsed.args.host);
        assert!(!parsed.args.no_host);
        let full = TestCommand::try_parse_from(["test"]).unwrap();
        assert!(!full.args.host);
        assert!(TestCommand::try_parse_from(["test", "--host", "--no-host"]).is_err());
    }

    #[test]
    fn takes_emulated_isolation_without_host() {
        let parsed = TestCommand::try_parse_from(["test", "--emulated-isolation"]).unwrap();
        assert!(parsed.args.emulated_isolation);
        let full = TestCommand::try_parse_from(["test"]).unwrap();
        assert!(!full.args.emulated_isolation);
        let both = ["test", "--host", "--emulated-isolation"];
        assert!(TestCommand::try_parse_from(both).is_err());
    }

    #[test]
    fn passes_only_on_the_exit_code_without_a_panic() {
        assert_eq!(verdict(Some(PASSED), "TEST a PASSED\n"), Ok(()));
        assert!(verdict(Some(35), "").is_err());
        assert!(verdict(None, "").is_err());
        assert!(verdict(Some(PASSED), "x\nKERNEL PANIC: oops\n").is_err());
    }

    #[test]
    fn reports_the_tests_and_the_end_of_a_timed_out_run() {
        let mut serial = String::from("TEST a PASSED\n");
        for i in 0..TAIL_LINES + 5 {
            serial.push_str(&format!("line {i}\n"));
        }
        let report = timeout_report(&serial);
        let lines: Vec<&str> = report.lines().collect();
        assert_eq!(lines[0], "TEST a PASSED");
        assert_eq!(lines[1], format!("last {TAIL_LINES} serial lines:"));
        assert_eq!(lines[2], "line 5");
        assert_eq!(
            lines.last(),
            Some(&format!("line {}", TAIL_LINES + 4).as_str())
        );
        assert_eq!(timeout_report(""), "last 0 serial lines:\n");
    }

    #[test]
    fn finds_the_interrupt_code_and_its_closures() {
        let symbols = "\
ffffffff8006b040 0000000000000219 t kernel::idt::interrupt
ffffffff8006aa30 00000000000001c3 t kernel::idt::after_interrupt
ffffffff800929f0 0000000000000015 t kernel::timer::interrupt
ffffffff8006fc40 00000000000003fb t kernel_core::sched::timer_interrupt
ffffffff80070a30 0000000000000140 t kernel_core::sched::maintain
ffffffff800cc860 0000000000000054 t kernel_core::sched::maintain::{closure#0}
ffffffff800f9690 000000000000007a t <core::option::Option<i64>>::map_or::<i64, kernel_core::sched::maintain::{closure#0}>
ffffffff8005be30 0000000000000010 t kernel::time::maintain
ffffffff800ccff0 000000000000008b t <kernel_core::ktimer::Armed>::fire
";
        let ranges = code_ranges(symbols, &INTERRUPT_FUNCTIONS).unwrap();
        assert_eq!(ranges.len(), 7);
        assert_eq!(ranges[0], (0xffffffff8006b040, 0xffffffff8006b259));
        assert_eq!(ranges[4], (0xffffffff800cc860, 0xffffffff800cc8b4));
        assert_eq!(
            code_arg("interrupt_code", &ranges[..2]),
            "interrupt_code=ffffffff8006b040-ffffffff8006b259,ffffffff800929f0-ffffffff80092a05"
        );
        let without = symbols.replace("kernel::time::maintain", "kernel::time::other");
        assert_eq!(
            code_ranges(&without, &INTERRUPT_FUNCTIONS),
            Err("kernel::time::maintain")
        );
        assert_eq!(
            code_ranges(symbols, &IDLE_FUNCTIONS),
            Err("kernel_core::sched::halt_until_interrupt")
        );
    }

    #[test]
    fn scratch_disks_hold_one_partition_over_every_usable_block() {
        let dir = std::env::temp_dir().join(format!("scratch-disks-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        scratch_disks(&dir.join("serial.log")).unwrap();
        for (name, _, partition_guid) in SCRATCH_DISKS {
            let d = fs::read(dir.join(name)).unwrap();
            assert_eq!(d.len(), 1 << 20);
            let u64_at = |o: usize| u64::from_le_bytes(d[o..o + 8].try_into().unwrap());
            assert_eq!(&d[510..512], &[0x55, 0xaa]);
            assert_eq!(&d[512..520], b"EFI PART");
            assert_eq!(&d[2047 * 512..2047 * 512 + 8], b"EFI PART");
            // The primary header's last usable LBA, and the one entry.
            assert_eq!(u64_at(512 + 48), 2014);
            let entry = &d[1024..1024 + 128];
            let guid = uuid::Uuid::from_bytes_le(entry[16..32].try_into().unwrap());
            assert_eq!(guid.to_string(), partition_guid);
            let ty = uuid::Uuid::from_bytes_le(entry[0..16].try_into().unwrap());
            assert_eq!(ty.to_string(), SCRATCH_TYPE);
            assert_eq!(u64_at(1024 + 32), 34);
            assert_eq!(u64_at(1024 + 40), 2014);
            assert!(d[1024 + 128..1024 + 256].iter().all(|&b| b == 0));
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writes_serial_output_to_the_log() {
        let args = vec!["-m".into(), "1G".into(), "-serial".into(), "stdio".into()];
        let args = test_args(args, Path::new("/t/serial.log"), false);
        assert_eq!(args[3], "file:/t/serial.log");
        assert!(args.contains(&"isa-debug-exit,iobase=0xf4,iosize=0x04".to_string()));
        assert!(args.contains(&"edu,dma_mask=0xffffffff".to_string()));
        assert!(args.contains(&"if=none,id=scratch-nvme,format=raw,file=/t/nvme.img".to_string()));
        assert!(args.contains(&"nvme,drive=scratch-nvme,serial=mikros".to_string()));
        assert!(args.contains(&"if=none,id=scratch-ahci,format=raw,file=/t/ahci.img".to_string()));
        assert!(args.contains(&"ide-hd,drive=scratch-ahci,bus=ide.2,serial=mikros".to_string()));
        assert!(args.contains(&"qemu-xhci,id=usb,addr=0x1d".to_string()));
        assert!(args.contains(&"if=none,id=scratch-usb,format=raw,file=/t/usb.img".to_string()));
        assert!(args.contains(
            &"usb-storage,id=stick,bus=usb.0,port=1,drive=scratch-usb,serial=mikros".to_string()
        ));
        assert!(args.contains(&"usb-hub,bus=usb.0,port=2".to_string()));
        assert!(args.contains(&"usb-kbd,id=usbkbd,bus=usb.0,port=2.1".to_string()));
        assert!(args.contains(&"i6300esb".to_string()));
        assert!(args.contains(&"-no-reboot".to_string()));
        assert!(!args.iter().any(|a| a.contains("scratch-virtio")));
    }

    #[test]
    fn logs_the_writes_of_the_scratch_disks_when_logged() {
        let args = vec!["-serial".into(), "stdio".into()];
        let args = test_args(args, Path::new("/t/serial.log"), true);
        assert!(!args.contains(&"-drive".to_string()));
        for (id, name) in [
            ("scratch-nvme", "nvme"),
            ("scratch-ahci", "ahci"),
            ("scratch-usb", "usb"),
            ("scratch-virtio", "virtio"),
        ] {
            let file = format!("driver=file,node-name={id}-file,filename=/t/{name}.img");
            let log = format!("driver=file,node-name={id}-log,filename=/t/{name}.writes");
            let logged = format!(
                "driver=blklogwrites,node-name={id},file={id}-file,log={id}-log,log-sector-size=512"
            );
            for arg in [file, log, logged] {
                assert!(args.contains(&arg), "{arg}");
            }
        }
        let virtio = "virtio-blk-pci,drive=scratch-virtio,disable-legacy=on,serial=mikros";
        assert!(args.contains(&virtio.to_string()));
        assert_eq!(args.last().map(String::as_str), Some("-no-reboot"));
    }
}
