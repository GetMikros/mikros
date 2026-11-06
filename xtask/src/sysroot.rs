//! `cargo xtask sysroot`: the directory that C and C++ programs for
//! Mikros are built against, laid out like `/usr`.
//!
//! It holds the musl headers in `include/`, with the generated
//! `<mikros/syscalls.h>`, `<mikros/abi.h>` and `<mikros/process.h>` in
//! `include/mikros/`;
//! `libc.slib`, `libc.dlib`, the dynamic linker `ld-mikros.dlib`, which
//! is the same file, `crt1.o`, `crti.o`, `crtn.o` and LLVM's static
//! unwinder `libunwind.slib` in `lib/`, with
//! `lib/pkgconfig/`, and the Clang driver configuration file
//! `x86_64-unknown-mikros.cfg`, which selects the target, its CPU level
//! and this sysroot. The Clang that `crate::llvm` builds, which knows the
//! target, is used with it as
//! `clang --config=<sysroot>/x86_64-unknown-mikros.cfg`.
//!
//! For ported projects, it holds the Meson cross file
//! `x86_64-unknown-mikros.meson`. It selects these compilers and this
//! sysroot, and works with the Meson that `crate::meson` patches, which
//! names shared libraries `lib<name>.<version>.dlib` with the
//! `DT_SONAME` `lib<name>.<major>.dlib`, links `lib<name>.dlib` to the
//! latter, and names static libraries `lib<name>.slib`.

use crate::llvm::{self, LLVM};
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The target triple of C and C++ code.
pub const TRIPLE: &str = "x86_64-unknown-mikros";

/// The directories of the sysroot, relative to it.
const DIRS: [&str; 4] = ["include", "include/mikros", "lib", "lib/pkgconfig"];

#[derive(clap::Args)]
pub struct Args {}

pub fn run(_args: &Args) -> Result<()> {
    let sysroot = build()?;
    println!("built    {}", sysroot.display());
    Ok(())
}

/// Where the sysroot is built.
pub fn dir() -> PathBuf {
    crate::workspace_root().join("target/sysroot")
}

/// Builds the toolchain and the sysroot, returning the sysroot's path.
pub fn build() -> Result<PathBuf> {
    llvm::build()?;
    check_llvm(&llvm::tool("clang"))?;
    check_llvm(&llvm::tool("ld.lld"))?;
    check_driver()?;
    let sysroot = dir();
    for d in DIRS {
        let path = sysroot.join(d);
        fs::create_dir_all(&path).with_context(|| format!("creating {}", path.display()))?;
    }
    let cfg = config(&sysroot);
    fs::write(&cfg, driver_config()).with_context(|| format!("writing {}", cfg.display()))?;
    let cross = meson_cross_file();
    fs::write(&cross, meson_cross(&llvm_from_sysroot()?))
        .with_context(|| format!("writing {}", cross.display()))?;
    let header = sysroot.join("include/mikros/syscalls.h");
    fs::write(&header, syscalls_header()?)
        .with_context(|| format!("writing {}", header.display()))?;
    for header in ["abi.h", "process.h"] {
        let from = crate::workspace_root()
            .join("user/libc/include/mikros")
            .join(header);
        copy(&from, &sysroot.join("include/mikros").join(header))?;
    }
    // The compiler-rt builtins are built against musl's headers, and
    // musl's libraries with the builtins.
    crate::musl::install_headers(&cfg, &sysroot)?;
    llvm::build_builtins(&sysroot)?;
    let layer = mikros_layer(false)?;
    let shared_layer = exported_object(&mikros_layer(true)?)?;
    let abi_note = crate::workspace_root().join("target/musl/abi-note.o");
    assemble(&cfg, "user/libc/crt/abi-note.S", &abi_note)?;
    let musl = crate::musl::build(&cfg, &[&shared_layer, &abi_note])?;
    let lib = sysroot.join("lib");
    combine(&lib.join("libc.slib"), &[&musl.libc, &layer])?;
    // The shared library is also the dynamic linker.
    copy(&musl.shared, &lib.join("libc.dlib"))?;
    copy(&musl.shared, &lib.join("ld-mikros.dlib"))?;
    // Every executable and shared library links crti.o, and so gets the
    // ABI note.
    crate::port::run(
        Command::new(llvm::tool("ld.lld"))
            .arg("-r")
            .arg(&musl.crti)
            .arg(&abi_note)
            .arg("-o")
            .arg(lib.join("crti.o")),
    )
    .context("adding the ABI note to crti.o")?;
    copy(&musl.crtn, &lib.join("crtn.o"))?;
    copy(&llvm::build_unwinder(&cfg)?, &lib.join("libunwind.slib"))?;
    assemble(&cfg, "user/libc/crt/crt1.S", &lib.join("crt1.o"))?;
    Ok(sysroot)
}

/// Assembles `source`, a path relative to the repository, to `object`
/// with the Clang configuration `cfg`.
fn assemble(cfg: &Path, source: &str, object: &Path) -> Result<()> {
    crate::port::run(
        Command::new(llvm::tool("clang"))
            .arg(format!("--config={}", cfg.display()))
            .arg("-c")
            .arg(crate::workspace_root().join(source))
            .arg("-o")
            .arg(object),
    )
    .with_context(|| format!("assembling {source}"))
}

/// Builds the Mikros layer of the C library, a static library of the
/// `user` workspace, and returns its path: with `shared`, the variant
/// for the shared library, in a target directory of its own.
fn mikros_layer(shared: bool) -> Result<PathBuf> {
    let root = crate::workspace_root();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let target_dir = if shared {
        root.join("user/target/libc-shared")
    } else {
        root.join("user/target")
    };
    let mut command = Command::new(cargo);
    command
        .current_dir(root.join("user"))
        .args(["build", "--release", "--package", "mikros-libc", "--lib"])
        .arg("--target-dir")
        .arg(&target_dir)
        .arg("--target")
        .arg(root.join(crate::build::USER_TARGET));
    if shared {
        command.args(["--features", "shared"]);
    }
    crate::port::run(&mut command).context("building the Mikros layer of the C library")?;
    Ok(target_dir.join("x86_64-unknown-mikros/release/libmikros_libc.slib"))
}

/// The members of the static library `archive` linked into one object,
/// in which only the `mikros_` and `__mikros_` functions stay global.
/// musl hides what the shared library takes from archives, so the
/// Mikros layer goes in as this object: its C functions are exported,
/// and the Rust code and runtime functions they use are not.
fn exported_object(archive: &Path) -> Result<PathBuf> {
    let object = archive.with_extension("o");
    crate::port::run(
        Command::new(llvm::tool("ld.lld"))
            .args(["-r", "--whole-archive"])
            .arg(archive)
            .arg("-o")
            .arg(&object),
    )
    .context("linking the Mikros layer into one object")?;
    crate::port::run(
        Command::new(llvm::tool("llvm-objcopy"))
            .args([
                "--wildcard",
                "--keep-global-symbol=mikros_*",
                "--keep-global-symbol=__mikros_*",
            ])
            .arg(&object),
    )
    .context("hiding the Rust symbols of the Mikros layer")?;
    Ok(object)
}

/// Writes the static library `to` with the members of every archive of
/// `from`.
fn combine(to: &Path, from: &[&Path]) -> Result<()> {
    let mut script = format!("create {}\n", to.display());
    for archive in from {
        script += &format!("addlib {}\n", archive.display());
    }
    script += "save\nend\n";
    let mut child = Command::new(llvm::tool("llvm-ar"))
        .arg("-M")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("running llvm-ar")?;
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(script.as_bytes())?;
    if !child.wait()?.success() {
        bail!("combining the archives of {} failed", to.display());
    }
    Ok(())
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to)
        .map(drop)
        .with_context(|| format!("copying {} to {}", from.display(), to.display()))
}

/// `<mikros/syscalls.h>`, generated from the system call table.
fn syscalls_header() -> Result<String> {
    let path = crate::workspace_root().join("kernel/syscalls.yaml");
    let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let read = syscall_table::table::read(&bytes)
        .map_err(|e| anyhow::anyhow!("{}:{e}", path.display()))?;
    Ok(syscall_table::c::header(&read.table, "x86_64"))
}

/// The Clang driver configuration file of `sysroot`.
pub fn config(sysroot: &Path) -> PathBuf {
    sysroot.join(format!("{TRIPLE}.cfg"))
}

/// The Meson cross file of the sysroot.
pub fn meson_cross_file() -> PathBuf {
    dir().join(format!("{TRIPLE}.meson"))
}

/// The directory of the LLVM programs relative to the sysroot, beside
/// which the LLVM directory lies.
fn llvm_from_sysroot() -> Result<String> {
    let bin = llvm::bin();
    match dir().parent().map(|parent| bin.strip_prefix(parent)) {
        Some(Ok(relative)) => Ok(format!("../{}", relative.display())),
        _ => bail!("{} lies apart from the sysroot", bin.display()),
    }
}

/// The Meson cross file, with the LLVM programs in `llvm`, relative to
/// the sysroot. Meson replaces `@DIRNAME@` with the file's directory,
/// the sysroot.
fn meson_cross(llvm: &str) -> String {
    let tool = |name: &str| format!("'@DIRNAME@/{llvm}/{name}'");
    let compiler = |name: &str| format!("[{}, '--config=@DIRNAME@/{TRIPLE}.cfg']", tool(name));
    format!(
        "# The Mikros target for Meson, written by cargo xtask sysroot, for the
# Meson with the Mikros patches that cargo xtask runs:
#   meson setup --cross-file <sysroot>/{TRIPLE}.meson ...
# The system mikros names the libraries.

[binaries]
c = {}
cpp = {}
ar = {}
ranlib = {}

[properties]
sys_root = '@DIRNAME@'

[host_machine]
system = 'mikros'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'
",
        compiler("clang"),
        compiler("clang++"),
        tool("llvm-ar"),
        tool("llvm-ranlib"),
    )
}

/// Fails unless `tool --version` reports the pinned LLVM release.
fn check_llvm(tool: &Path) -> Result<()> {
    let shown = tool.display();
    let output = Command::new(tool)
        .arg("--version")
        .output()
        .with_context(|| format!("running {shown} --version"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    match crate::tools::Version::find_in(&text) {
        Some(found) if found == LLVM => Ok(()),
        Some(found) => bail!("{shown} is LLVM {found}, but Mikros pins LLVM {LLVM}"),
        None => bail!("{shown} --version names no version"),
    }
}

/// Fails unless Clang compiles and links for the target with the
/// settings it must have: position-independent code with the strong
/// stack protector, linked by LLD as a position-independent executable
/// with the Mikros interpreter.
fn check_driver() -> Result<()> {
    let output = Command::new(llvm::tool("clang"))
        .args([
            &format!("--target={TRIPLE}"),
            "-###",
            "-x",
            "c",
            "-",
            "-o",
            "a.out",
        ])
        .output()
        .context("running clang -###")?;
    let text = String::from_utf8_lossy(&output.stderr);
    for wanted in [
        "\"-mrelocation-model\" \"pic\"",
        "\"-stack-protector\" \"2\"",
        "ld.lld\"",
        "\"-pie\"",
        "\"-dynamic-linker\" \"/usr/lib/ld-mikros.dlib\"",
        "crt1.o\"",
    ] {
        if !text.contains(wanted) {
            bail!("clang for {TRIPLE} lacks {wanted}:\n{text}");
        }
    }
    Ok(())
}

/// The Clang driver configuration of the target. `<CFGDIR>` is the
/// directory of the file, which is the sysroot. The rest of the target's
/// settings are the defaults of Clang's Mikros toolchain.
fn driver_config() -> String {
    [
        "# The Mikros target, written by cargo xtask sysroot.",
        &format!("--target={TRIPLE}"),
        "-march=x86-64-v3",
        "--sysroot=<CFGDIR>",
        "",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_selects_the_target_and_its_options() {
        let cfg = driver_config();
        for option in [
            "--target=x86_64-unknown-mikros",
            "-march=x86-64-v3",
            "--sysroot=<CFGDIR>",
        ] {
            assert!(cfg.lines().any(|l| l == option), "{option}");
        }
    }

    #[test]
    fn cross_file_selects_mikros_and_the_compilers() {
        let cross = meson_cross("../llvm/bin");
        for line in [
            "c = ['@DIRNAME@/../llvm/bin/clang', '--config=@DIRNAME@/x86_64-unknown-mikros.cfg']",
            "sys_root = '@DIRNAME@'",
            "system = 'mikros'",
        ] {
            assert!(cross.lines().any(|l| l == line), "{line}");
        }
    }
}
