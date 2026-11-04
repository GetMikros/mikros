//! `cargo xtask debug`: boots the disk image in QEMU halted, with its GDB
//! stub enabled, and starts GDB attached to it with the kernel's debug
//! information and the kernel debugger scripts. Also the check of those
//! scripts that `cargo xtask test` runs.

use crate::build::{self, Kind};
use crate::qemu::{self, MachineArgs};
use crate::{firmware, image};
use anyhow::{Context, Result, bail};
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The GDB that can debug x86_64 code on this host: `gdb-multiarch` when it
/// is installed, as Debian and Ubuntu need on hosts that are not x86_64,
/// and otherwise `gdb`.
pub fn gdb() -> &'static str {
    static GDB: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    GDB.get_or_init(|| {
        let multiarch = Command::new("gdb-multiarch")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if multiarch { "gdb-multiarch" } else { "gdb" }
    })
}

#[derive(clap::Args)]
pub struct Args {
    /// The build kind to debug.
    #[arg(long, value_enum, default_value_t = Kind::Debug)]
    pub kind: Kind,
    #[command(flatten)]
    pub machine: MachineArgs,
}

/// The kernel debugger scripts.
fn scripts() -> PathBuf {
    crate::workspace_root().join("kernel/gdb/mikros.py")
}

/// A free TCP port on the loopback interface for QEMU's GDB stub.
pub fn free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("finding a free port")?;
    Ok(listener.local_addr()?.port())
}

/// The arguments of `qemu::args` with the serial port written to `log`
/// and the GDB stub listening on `port` with the processors halted.
fn halted_args(mut args: Vec<String>, log: &Path, port: u16) -> Vec<String> {
    if let Some(i) = args.iter().position(|a| a == "-serial") {
        args[i + 1] = format!("file:{}", log.display());
    }
    args.extend(["-gdb".into(), format!("tcp:127.0.0.1:{port}"), "-S".into()]);
    args
}

/// The GDB arguments that load the kernel's symbols, whose debug
/// information was split into the build ID directory beside it, source
/// the scripts and attach to the stub on `port`.
fn gdb_args(kernel: &Path, port: u16) -> Vec<String> {
    let dir = kernel.parent().unwrap_or(Path::new("."));
    [
        format!("set debug-file-directory {}", dir.display()),
        format!("file {}", kernel.display()),
        format!("source {}", scripts().display()),
        format!("target remote 127.0.0.1:{port}"),
    ]
    .into_iter()
    .flat_map(|command| ["-ex".to_string(), command])
    .collect()
}

/// Starts QEMU halted on `disk` with the GDB stub on `port` and the
/// serial output in `log`, and waits until the stub accepts connections.
pub fn start_halted(
    machine: &MachineArgs,
    disk: &Path,
    log: &Path,
    port: u16,
    extra: &[&str],
) -> Result<Child> {
    let firmware = firmware::paths();
    if !firmware.code.exists() {
        bail!("the firmware is missing; fetch it with `cargo xtask setup`");
    }
    let vars = disk.with_extension("vars.fd");
    fs::copy(&firmware.vars, &vars)
        .with_context(|| format!("copying the firmware variables to {}", vars.display()))?;
    let kvm = qemu::kvm_available();
    let mut args = halted_args(qemu::args(machine, kvm, &firmware, &vars, disk), log, port);
    args.extend(extra.iter().map(|a| a.to_string()));
    // In a process group of its own, QEMU does not get the SIGINT that
    // Ctrl-C sends to the terminal's foreground group, which GDB uses to
    // stop the kernel. It then no longer goes with this process on the
    // terminal's signals, so it is killed when this process exits.
    let mut command = Command::new(qemu::QEMU);
    command.args(args).stdin(Stdio::null()).process_group(0);
    // SAFETY: the closure only makes a system call, which is safe
    // between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        })
    };
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start {}", qemu::QEMU))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        if let Some(status) = child.try_wait()? {
            bail!("QEMU exited with {status} before its GDB stub started");
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("QEMU's GDB stub did not start");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(child)
}

pub fn run(args: &Args) -> Result<()> {
    let built = build::build(args.kind)?;
    let disk = image::path(args.kind, false);
    image::create(args.kind, &built, &disk)?;
    let log = disk.with_extension("serial.log");
    let port = free_port()?;
    let mut qemu = start_halted(&args.machine, &disk, &log, port, &[])?;
    println!("serial output goes to {}", log.display());
    let status = Command::new(gdb())
        .args(gdb_args(&built.kernel, port))
        .spawn()
        .and_then(|mut gdb| {
            // Ctrl-C is meant for GDB, which shares the terminal's
            // foreground group with this process. GDB was started before
            // the signal is ignored here, so it keeps the default action.
            // SAFETY: SIG_IGN installs no handler; nothing else here uses
            // SIGINT.
            unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
            gdb.wait()
        })
        .with_context(|| format!("failed to start {}", gdb()));
    let _ = qemu.kill();
    let _ = qemu.wait();
    let status = status?;
    if !status.success() {
        bail!("GDB exited with {status}");
    }
    Ok(())
}

/// The function the scripts check stops in: the test init first calls
/// it in `thread_by_koid`, while a helper thread of it is switched out
/// and it has handles and mappings of every kind.
const CHECK_BREAKPOINT: &str = "kernel::syscall::handlers::process_get_thread";

/// The commands the check runs, each with the texts its output must
/// contain.
const CHECKS: [(&str, &[&str]); 5] = [
    ("mikros ps", &["RUNNING"]),
    (
        "mikros threads init",
        &[
            "running; its registers are GDB's",
            "kernel rsp=0x",
            " rip=0x",
        ],
    ),
    ("mikros handles init", &["Process"]),
    ("mikros mappings init", &["r-x"]),
    ("mikros log 20", &["TEST "]),
];

/// Waits until `child` exits, or kills it and fails once `timeout` has
/// passed.
pub fn wait_for(child: &mut Child, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {} s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Boots the test image `disk` halted, runs every kernel debugger script
/// command in GDB once the test init calls `CHECK_BREAKPOINT`, and fails
/// unless each prints what it should, or when GDB has not finished after
/// `timeout`. The serial output goes to `log`, and GDB's output to
/// `gdb-output.log` next to it.
pub fn check_scripts(
    machine: &MachineArgs,
    kernel: &Path,
    disk: &Path,
    log: &Path,
    timeout: Duration,
) -> Result<()> {
    let port = free_port()?;
    let mut qemu = start_halted(machine, disk, log, port, &["-display", "none"])?;
    let mut args = vec!["-nx".to_string(), "-batch".to_string()];
    args.extend(gdb_args(kernel, port));
    args.extend(["-ex".into(), format!("hbreak {CHECK_BREAKPOINT}")]);
    args.extend(["-ex".into(), "continue".into()]);
    for (command, _) in CHECKS {
        args.extend(["-ex".into(), format!("echo @@ {command}\\n")]);
        args.extend(["-ex".into(), command.into()]);
    }
    args.extend(["-ex".into(), "kill".into()]);
    // The output goes to a file, so that waiting never blocks on a pipe.
    let output_path = log.with_file_name("gdb-output.log");
    let output = fs::File::create(&output_path)
        .with_context(|| format!("creating {}", output_path.display()))?;
    let errors = output.try_clone()?;
    let finished = Command::new(gdb())
        .args(args)
        .stdin(Stdio::null())
        .stdout(output)
        .stderr(errors)
        .spawn()
        .with_context(|| format!("failed to start {}", gdb()))
        .and_then(|mut gdb| wait_for(&mut gdb, timeout));
    let _ = qemu.kill();
    let _ = qemu.wait();
    finished.with_context(|| {
        format!(
            "the kernel debugger scripts did not finish; serial output in {}",
            log.display()
        )
    })?;
    let output = fs::read_to_string(&output_path)
        .with_context(|| format!("reading {}", output_path.display()))?;
    let failed = missing(&output);
    if !failed.is_empty() {
        bail!(
            "the kernel debugger scripts failed: {}\n{output}",
            failed.join(", ")
        );
    }
    Ok(())
}

/// The commands of `CHECKS` whose output, between the `@@` markers in
/// `stdout`, lacks what it should contain.
fn missing(stdout: &str) -> Vec<&'static str> {
    CHECKS
        .iter()
        .filter(|(command, expected)| {
            let marker = format!("@@ {command}\n");
            let output = stdout.split_once(&marker).map(|(_, rest)| rest);
            let output = output.map(|rest| rest.split("@@ ").next().unwrap_or(""));
            !output.is_some_and(|o| expected.iter().all(|e| o.contains(e)))
        })
        .map(|(command, _)| *command)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halts_with_the_stub_on_the_port() {
        let args = vec!["-serial".into(), "stdio".into()];
        let args = halted_args(args, Path::new("/t/serial.log"), 4321);
        assert_eq!(
            args,
            [
                "-serial",
                "file:/t/serial.log",
                "-gdb",
                "tcp:127.0.0.1:4321",
                "-S"
            ]
        );
    }

    #[test]
    fn finds_each_command_output() {
        let good: String = CHECKS
            .iter()
            .map(|(c, e)| format!("@@ {c}\n{}\n", e.join("\n")))
            .collect();
        assert!(missing(&good).is_empty());
        let bad = good.replace("Process", "Thread");
        assert_eq!(missing(&bad), ["mikros handles init"]);
        let bad = good.replace("kernel rsp=", "kernel sp=");
        assert_eq!(missing(&bad), ["mikros threads init"]);
    }
}
