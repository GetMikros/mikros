//! `cargo xtask test`: runs the host unit tests, replays the host fuzz
//! corpora and runs the integration tests in QEMU.

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
    /// Skip the host unit tests and the replay of the fuzz corpora.
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

/// What the integration tests of `kind` add to the kernel command line:
/// for the fuzz build, the interrupt code and the idle code of `kernel`.
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
/// no display, the debug exit device, QEMU's `edu` device for DMA tests,
/// able to reach the first 4 GiB, and no reboot. The devices follow the
/// IOMMU, which must come first.
pub fn test_args(mut args: Vec<String>, log: &Path) -> Vec<String> {
    if let Some(i) = args.iter().position(|a| a == "-serial") {
        args[i + 1] = format!("file:{}", log.display());
    }
    args.extend(
        [
            "-display",
            "none",
            "-device",
            "isa-debug-exit,iobase=0xf4,iosize=0x04",
            "-device",
            "edu,dma_mask=0xffffffff",
            "-no-reboot",
        ]
        .map(String::from),
    );
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
            let serial = fs::read(log).unwrap_or_default();
            print!("{}", timeout_report(&String::from_utf8_lossy(&serial)));
            bail!(
                "the integration tests timed out after {} s; serial output in {}",
                args.timeout,
                log.display()
            );
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
        crate::fuzz::replay_corpora()?;
    }

    let built = build::build_for(args.kind, BootImage::Test)?;
    let dir = test_dir(args.kind);
    fs::create_dir_all(&dir)?;
    let disk = dir.join("disk.img");
    let cmdline = test_cmdline(args.kind, &built.kernel)?;
    image::create_with(args.kind, &built, &disk, &cmdline, &[])?;
    let log = dir.join("serial.log");
    let code = boot(args, &disk, &log)?;
    let serial = fs::read(&log).with_context(|| format!("reading {}", log.display()))?;
    let serial = String::from_utf8_lossy(&serial);
    for line in serial.lines().filter(|l| l.contains("TEST")) {
        println!("{line}");
    }
    if let Err(why) = verdict(code, &serial) {
        bail!(
            "integration tests failed: {why}; serial output in {}",
            log.display()
        );
    }
    println!(
        "integration tests passed; serial output in {}",
        log.display()
    );
    // The debugger checks boot as cargo xtask debug does, at the kernel's
    // linked addresses.
    let gdb_disk = dir.join("gdb-disk.img");
    let gdb_cmdline = [cmdline.as_str(), image::KASLR_OFF].join(" ");
    image::create_with(args.kind, &built, &gdb_disk, gdb_cmdline.trim(), &[])?;
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
    Ok(())
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
    fn writes_serial_output_to_the_log() {
        let args = vec!["-m".into(), "1G".into(), "-serial".into(), "stdio".into()];
        let args = test_args(args, Path::new("/t/serial.log"));
        assert_eq!(args[3], "file:/t/serial.log");
        assert!(args.contains(&"isa-debug-exit,iobase=0xf4,iosize=0x04".to_string()));
        assert!(args.contains(&"edu,dma_mask=0xffffffff".to_string()));
        assert!(args.contains(&"-no-reboot".to_string()));
    }
}
