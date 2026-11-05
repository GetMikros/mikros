//! `cargo xtask build`: builds the system components.

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
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
            Kind::Debug => "dev",
            Kind::Fuzz => "fuzz",
        }
    }

    /// The directory Cargo writes the profile's output to.
    fn profile_dir(self) -> &'static str {
        match self {
            Kind::Production => "release",
            Kind::Debug => "debug",
            Kind::Fuzz => "fuzz",
        }
    }
}

#[derive(clap::Args)]
pub struct Args {
    /// The build kind.
    #[arg(long, value_enum, default_value_t = Kind::Debug)]
    pub kind: Kind,
}

/// The paths of the built components.
pub struct Built {
    pub bootmenu: PathBuf,
    pub bootloader: PathBuf,
    pub kernel: PathBuf,
    pub boot_image: PathBuf,
}

/// The boot images that can be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootImage {
    /// The system's boot image.
    System,
    /// The boot image of the integration tests, whose init runs them.
    Test,
}

/// Builds every component of the build kind `kind`.
pub fn build(kind: Kind) -> Result<Built> {
    build_for(kind, BootImage::System)
}

/// Builds every component of the build kind `kind`, with `boot` as the
/// boot image.
pub fn build_for(kind: Kind, boot: BootImage) -> Result<Built> {
    let bootmenu = cargo_build("bootmenu", "bootmenu", "x86_64-unknown-uefi", kind)?;
    println!("built    {}", bootmenu.display());
    let bootloader = cargo_build("mikrosboot", "mikrosboot", "x86_64-unknown-uefi", kind)?;
    println!("built    {}", bootloader.display());
    crate::loader_check::check(&bootloader)?;
    println!(
        "checked  {} holds no MP services protocol GUID",
        bootloader.display()
    );
    let kernel = cargo_build("kernel-x86_64", "kernel", KERNEL_TARGET, kind)?;
    println!("built    {}", kernel.display());
    crate::extended::check(&kernel)?;
    println!("checked  {} uses no x87 or vector state", kernel.display());
    if kind == Kind::Debug {
        crate::debug_build::check(&kernel)?;
        println!(
            "checked  {} has the debug build's settings",
            kernel.display()
        );
    }
    let debug_info = crate::debuginfo::split(&kernel)?;
    println!("built    {}", debug_info.display());
    if kind == Kind::Fuzz {
        let dir = syzkaller_descriptions()?;
        println!("built    {}", dir.display());
    }
    let boot_image = match boot {
        BootImage::System => {
            let init = user_build("init", kind)?;
            println!("built    {}", init.display());
            let manager = user_build("process-manager", kind)?;
            println!("built    {}", manager.display());
            let services = user_build("service-manager", kind)?;
            println!("built    {}", services.display());
            let members = [
                ("init", init.as_path()),
                ("usr/bin/process-manager", manager.as_path()),
                ("usr/bin/service-manager", services.as_path()),
            ];
            boot_image(kind, "boot.img", &members)?
        }
        BootImage::Test => {
            let init = user_build("test-init", kind)?;
            println!("built    {}", init.display());
            let echo = user_build("channel-echo", kind)?;
            println!("built    {}", echo.display());
            let server = user_build("call-server", kind)?;
            println!("built    {}", server.display());
            let debuggee = user_build("debuggee", kind)?;
            println!("built    {}", debuggee.display());
            let pie = user_build("pie", kind)?;
            println!("built    {}", pie.display());
            let managed = user_build("managed", kind)?;
            println!("built    {}", managed.display());
            let manager = user_build("process-manager", kind)?;
            println!("built    {}", manager.display());
            let services = user_build("service-manager", kind)?;
            println!("built    {}", services.display());
            let probe = user_build("launcher-probe", kind)?;
            println!("built    {}", probe.display());
            // The process manager tests serve this image, in which the
            // probe takes the service manager's place.
            let launcher = boot_image(
                kind,
                "launcher.img",
                &[
                    ("usr/bin/service-manager", probe.as_path()),
                    ("usr/bin/probe", probe.as_path()),
                ],
            )?;
            println!("built    {}", launcher.display());
            let broker = user_build("permissions-broker", kind)?;
            println!("built    {}", broker.display());
            let broker_probe = user_build("broker-probe", kind)?;
            println!("built    {}", broker_probe.display());
            // The broker tests serve this image, in which the broker
            // probe takes the service manager's place.
            let broker_image = boot_image(
                kind,
                "broker.img",
                &[
                    ("usr/bin/service-manager", broker_probe.as_path()),
                    ("usr/bin/permissions-broker", broker.as_path()),
                ],
            )?;
            println!("built    {}", broker_image.display());
            let members = [
                ("tests/broker.img", broker_image.as_path()),
                ("init", init.as_path()),
                ("tests/launcher.img", launcher.as_path()),
                ("tests/channel-echo", echo.as_path()),
                ("tests/call-server", server.as_path()),
                ("tests/debuggee", debuggee.as_path()),
                ("tests/pie", pie.as_path()),
                ("tests/managed", managed.as_path()),
                ("usr/bin/process-manager", manager.as_path()),
                ("usr/bin/service-manager", services.as_path()),
            ];
            boot_image(kind, "test-boot.img", &members)?
        }
    };
    println!("built    {}", boot_image.display());
    Ok(Built {
        bootmenu,
        bootloader,
        kernel,
        boot_image,
    })
}

/// Builds the boot image of a live system for `kind`: `init`, and the
/// directories of the root file system at their paths there, which hold
/// the whole system as it stands.
pub fn live_boot_image(kind: Kind) -> Result<PathBuf> {
    let init = user_build("init", kind)?;
    let dirs = crate::image::live_dirs();
    let dirs: Vec<&str> = dirs.iter().map(String::as_str).collect();
    archive(kind, "live-boot.img", &[("init", &init)], &dirs)
}

/// Writes the boot archive `file` with each of `members`, given by name
/// and the path of its contents, as a regular file.
fn boot_image(kind: Kind, file: &str, members: &[(&str, &Path)]) -> Result<PathBuf> {
    archive(kind, file, members, &[])
}

/// Like `boot_image`, with the directories `dirs` after the files.
fn archive(kind: Kind, file: &str, members: &[(&str, &Path)], dirs: &[&str]) -> Result<PathBuf> {
    let dir = crate::workspace_root()
        .join("target/boot")
        .join(kind.name());
    fs::create_dir_all(&dir)?;
    let path = dir.join(file);
    let file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    let mut archive = tar::Builder::new(file);
    for &(name, source) in members {
        let data = fs::read(source).with_context(|| format!("reading {}", source.display()))?;
        let mut header = tar::Header::new_ustar();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        archive
            .append_data(&mut header, name, data.as_slice())
            .with_context(|| format!("adding {name} to the boot archive"))?;
    }
    for &dir in dirs {
        let mut header = tar::Header::new_ustar();
        header.set_size(0);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Directory);
        archive
            .append_data(&mut header, dir_name(dir), std::io::empty())
            .with_context(|| format!("adding {dir} to the boot archive"))?;
    }
    archive.finish().context("finishing the boot archive")?;
    Ok(path)
}

/// The archive name of directory `dir`, which ends in a slash.
fn dir_name(dir: &str) -> String {
    format!("{dir}/")
}

pub fn run(args: &Args) -> Result<()> {
    build(args.kind).map(drop)
}

/// The target the kernel is built for.
pub const KERNEL_TARGET: &str = "x86_64-unknown-none";

/// The target user-space programs are built for.
pub const USER_TARGET: &str = "targets/x86_64-unknown-mikros.json";

/// Builds the user-space program `bin`, a package of the `user`
/// workspace of the same name, and returns its path.
fn user_build(bin: &str, kind: Kind) -> Result<PathBuf> {
    let root = crate::workspace_root();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    // The user workspace has its own configuration, which Cargo reads
    // from the directory it runs in.
    let mut command = Command::new(cargo);
    command
        .current_dir(root.join("user"))
        .args(["build", "--package", bin, "--bin", bin, "--target"])
        .arg(root.join(USER_TARGET))
        .args(["--profile", kind.profile()]);
    // The test init of a fuzz build also tests the fuzz-only calls.
    if matches!(kind, Kind::Fuzz) && bin == "test-init" {
        command.args(["--features", "fuzz"]);
    }
    let status = command.status().context("failed to run cargo")?;
    if !status.success() {
        bail!("building {bin} failed");
    }
    Ok(root
        .join("user/target/x86_64-unknown-mikros")
        .join(kind.profile_dir())
        .join(bin))
}

/// Writes the syzkaller descriptions and constants generated from the
/// system call table to `target/syzkaller/mikros`, and returns that
/// directory.
fn syzkaller_descriptions() -> Result<PathBuf> {
    let root = crate::workspace_root();
    let path = root.join("kernel/syscalls.yaml");
    let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let read = syscall_table::table::read(&bytes)
        .map_err(|e| anyhow::anyhow!("kernel/syscalls.yaml:{e}"))?;
    let dir = root.join("target/syzkaller/mikros");
    fs::create_dir_all(&dir)?;
    let table = &read.table;
    fs::write(
        dir.join("sys.txt"),
        syscall_table::syzkaller::descriptions(table, "x86_64"),
    )?;
    fs::write(
        dir.join("sys_amd64.const"),
        syscall_table::syzkaller::constants(table, "x86_64"),
    )?;
    Ok(dir)
}

/// The LLVM options that make every basic block call
/// `__sanitizer_cov_trace_pc`, as Linux's kcov expects.
const COVERAGE_FLAGS: [&str; 3] = [
    "-Cpasses=sancov-module",
    "-Cllvm-args=-sanitizer-coverage-level=3",
    "-Cllvm-args=-sanitizer-coverage-trace-pc",
];

/// The extra Cargo arguments for building `package` as `kind`: the fuzz
/// build of the kernel gets its fuzz feature and coverage
/// instrumentation, added to the kernel target's flags.
fn kernel_fuzz_args(package: &str, kind: Kind) -> Vec<String> {
    if package != "kernel-x86_64" || kind != Kind::Fuzz {
        return Vec::new();
    }
    let flags: Vec<String> = COVERAGE_FLAGS.iter().map(|f| format!("\"{f}\"")).collect();
    vec![
        "--features".into(),
        "fuzz".into(),
        "--config".into(),
        format!("target.{KERNEL_TARGET}.rustflags=[{}]", flags.join(", ")),
    ]
}

/// Builds the binary `bin` of `package` for `target` and returns its path.
fn cargo_build(package: &str, bin: &str, target: &str, kind: Kind) -> Result<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .current_dir(crate::workspace_root())
        .args([
            "build",
            "--package",
            package,
            "--bin",
            bin,
            "--target",
            target,
        ])
        .args(["--profile", kind.profile()])
        .args(kernel_fuzz_args(package, kind))
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
        .join(bin);
    path.set_extension(extension);
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruments_only_the_fuzz_kernel() {
        assert!(kernel_fuzz_args("kernel-x86_64", Kind::Debug).is_empty());
        assert!(kernel_fuzz_args("mikrosboot", Kind::Fuzz).is_empty());
        let args = kernel_fuzz_args("kernel-x86_64", Kind::Fuzz);
        assert_eq!(args[..3], ["--features", "fuzz", "--config"]);
        assert!(
            args[3].starts_with("target.x86_64-unknown-none.rustflags=[\"-Cpasses=sancov-module\"")
        );
    }

    #[test]
    fn builds_the_boot_archive() {
        let dir = crate::workspace_root().join("target/xtask-test");
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("init");
        fs::write(&source, b"\x7fELF test").unwrap();
        let path = boot_image(Kind::Fuzz, "boot.img", &[("init", &source)]).unwrap();
        assert!(path.ends_with("target/boot/fuzz/boot.img"));
        let bytes = fs::read(&path).unwrap();
        let mut archive = tar::Archive::new(bytes.as_slice());
        let mut entries = archive.entries().unwrap();
        let mut init = entries.next().unwrap().unwrap();
        assert_eq!(init.path().unwrap().to_str(), Some("init"));
        assert_eq!(init.header().entry_type(), tar::EntryType::Regular);
        assert_eq!(&init.header().as_bytes()[257..263], b"ustar\0");
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut init, &mut data).unwrap();
        assert_eq!(data, b"\x7fELF test");
        assert!(entries.next().is_none());
    }
}
