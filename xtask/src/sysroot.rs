//! `cargo xtask sysroot`: the directory that C and C++ programs for
//! Mikros are built against, laid out like `/usr`.
//!
//! It holds the musl headers in `include/`, with the generated
//! `<mikros/syscalls.h>`, `<mikros/abi.h>`, `<mikros/process.h>` and
//! `<mikros/startup.h>` in `include/mikros/`;
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
//! `x86_64-unknown-mikros.meson` and the CMake toolchain file
//! `x86_64-unknown-mikros.cmake`, with the CMake platform module
//! `share/cmake/Platform/Mikros.cmake`. They select these compilers and
//! this sysroot, and work with the Meson and CMake that `crate::meson`
//! and `crate::cmake` patch, which name shared libraries
//! `lib<name>.<version>.dlib` with the `DT_SONAME`
//! `lib<name>.<major>.dlib`, link `lib<name>.dlib` to the latter, and
//! name static libraries `lib<name>.slib`. A shared library without a
//! version has the major version 0; loadable modules keep the names the
//! project gives them.

use crate::llvm::{self, LLVM};
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The target triple of C and C++ code.
pub const TRIPLE: &str = "x86_64-unknown-mikros";

/// The directories of the sysroot, relative to it.
const DIRS: [&str; 5] = [
    "include",
    "include/mikros",
    "lib",
    "lib/pkgconfig",
    "share/cmake/Platform",
];

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
/// Nothing the sysroot is made from changes while xtask runs, so it is
/// built once per run.
pub fn build() -> Result<PathBuf> {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    if let Some(sysroot) = BUILT.get() {
        return Ok(sysroot.clone());
    }
    let sysroot = make()?;
    Ok(BUILT.get_or_init(|| sysroot).clone())
}

/// Builds the sysroot. Its files are written only when their contents
/// change, so that what is built from them is not built again.
fn make() -> Result<PathBuf> {
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
    write(&cfg, driver_config().as_bytes())?;
    let llvm = llvm_from_sysroot()?;
    for (file, contents) in [
        (meson_cross_file(), meson_cross(&llvm)),
        (cmake_toolchain_file(), cmake_toolchain(&llvm)),
        (
            sysroot.join("share/cmake/Platform/Mikros.cmake"),
            CMAKE_PLATFORM.to_owned(),
        ),
    ] {
        write(&file, contents.as_bytes())?;
    }
    let header = sysroot.join("include/mikros/syscalls.h");
    write(&header, syscalls_header()?.as_bytes())?;
    for header in ["abi.h", "process.h", "startup.h"] {
        let from = crate::workspace_root()
            .join("user/runtime/libc/include/mikros")
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
    assemble(&cfg, "user/runtime/libc/crt/abi-note.S", &abi_note)?;
    let musl = crate::musl::build(&cfg, &[&shared_layer, &abi_note])?;
    let lib = sysroot.join("lib");
    combine(&lib.join("libc.slib"), &[&musl.libc, &layer])?;
    // The shared library is also the dynamic linker.
    copy(&musl.shared, &lib.join("libc.dlib"))?;
    copy(&musl.shared, &lib.join("ld-mikros.dlib"))?;
    // Every executable and shared library links crti.o, and so gets the
    // ABI note.
    let crti = lib.join("crti.o");
    crate::port::run(
        Command::new(llvm::tool("ld.lld"))
            .arg("-r")
            .arg(&musl.crti)
            .arg(&abi_note)
            .arg("-o")
            .arg(fresh(&crti)),
    )
    .context("adding the ABI note to crti.o")?;
    keep_if_unchanged(&crti)?;
    copy(&musl.crtn, &lib.join("crtn.o"))?;
    copy(&llvm::build_unwinder(&cfg)?, &lib.join("libunwind.slib"))?;
    assemble(&cfg, "user/runtime/libc/crt/crt1.S", &lib.join("crt1.o"))?;
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
            .arg(fresh(object)),
    )
    .with_context(|| format!("assembling {source}"))?;
    keep_if_unchanged(object)
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
    let linked = fresh(&object);
    crate::port::run(
        Command::new(llvm::tool("ld.lld"))
            .args(["-r", "--whole-archive"])
            .arg(archive)
            .arg("-o")
            .arg(&linked),
    )
    .context("linking the Mikros layer into one object")?;
    crate::port::run(
        Command::new(llvm::tool("llvm-objcopy"))
            .args([
                "--wildcard",
                "--keep-global-symbol=mikros_*",
                "--keep-global-symbol=__mikros_*",
            ])
            .arg(&linked),
    )
    .context("hiding the Rust symbols of the Mikros layer")?;
    keep_if_unchanged(&object)?;
    Ok(object)
}

/// Writes the static library `to` with the members of every archive of
/// `from`.
fn combine(to: &Path, from: &[&Path]) -> Result<()> {
    let mut script = format!("create {}\n", fresh(to).display());
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
    keep_if_unchanged(to)
}

/// Copies `from` to `to`, unless `to` already has its contents.
fn copy(from: &Path, to: &Path) -> Result<()> {
    let data = fs::read(from).with_context(|| format!("reading {}", from.display()))?;
    write(to, &data)
}

/// Writes `data` to `path`, unless `path` already holds it, so that its
/// modification time changes only with its contents.
fn write(path: &Path, data: &[u8]) -> Result<()> {
    if fs::read(path).is_ok_and(|old| old == data) {
        return Ok(());
    }
    fs::write(path, data).with_context(|| format!("writing {}", path.display()))
}

/// Where a tool writes the new version of `path`, which
/// `keep_if_unchanged` then moves to `path`.
fn fresh(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".new");
    PathBuf::from(name)
}

/// Replaces `path` with its new version from `fresh`, unless the two are
/// the same, in which case `path` and its modification time stay.
fn keep_if_unchanged(path: &Path) -> Result<()> {
    let new = fresh(path);
    let data = fs::read(&new).with_context(|| format!("reading {}", new.display()))?;
    if fs::read(path).is_ok_and(|old| old == data) {
        fs::remove_file(&new).with_context(|| format!("removing {}", new.display()))
    } else {
        fs::rename(&new, path)
            .with_context(|| format!("moving {} to {}", new.display(), path.display()))
    }
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

/// The CMake toolchain file of the sysroot.
pub fn cmake_toolchain_file() -> PathBuf {
    dir().join(format!("{TRIPLE}.cmake"))
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

/// The CMake toolchain file, with the LLVM programs in `llvm`, relative
/// to the sysroot.
fn cmake_toolchain(llvm: &str) -> String {
    format!(
        "# The Mikros target for CMake, written by cargo xtask sysroot, for the
# CMake with the Mikros patches that cargo xtask builds:
#   cmake -DCMAKE_TOOLCHAIN_FILE=<sysroot>/{TRIPLE}.cmake ...
# The platform module share/cmake/Platform/Mikros.cmake names the
# libraries.
set(CMAKE_SYSTEM_NAME Mikros)
set(CMAKE_SYSTEM_PROCESSOR x86_64)
list(APPEND CMAKE_MODULE_PATH \"${{CMAKE_CURRENT_LIST_DIR}}/share/cmake\")

set(CMAKE_SYSROOT \"${{CMAKE_CURRENT_LIST_DIR}}\")
set(CMAKE_FIND_ROOT_PATH \"${{CMAKE_CURRENT_LIST_DIR}}\")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)

get_filename_component(_MIKROS_LLVM \"${{CMAKE_CURRENT_LIST_DIR}}/{llvm}\" ABSOLUTE)
set(_MIKROS_CONFIG \"--config=${{CMAKE_CURRENT_LIST_DIR}}/{TRIPLE}.cfg\")
set(CMAKE_C_COMPILER \"${{_MIKROS_LLVM}}/clang\" \"${{_MIKROS_CONFIG}}\")
set(CMAKE_CXX_COMPILER \"${{_MIKROS_LLVM}}/clang++\" \"${{_MIKROS_CONFIG}}\")
set(CMAKE_ASM_COMPILER \"${{_MIKROS_LLVM}}/clang\" \"${{_MIKROS_CONFIG}}\")
set(CMAKE_AR \"${{_MIKROS_LLVM}}/llvm-ar\")
set(CMAKE_RANLIB \"${{_MIKROS_LLVM}}/llvm-ranlib\")
"
    )
}

/// The CMake platform module of Mikros, which the toolchain file
/// selects.
const CMAKE_PLATFORM: &str = r#"# The Mikros platform for CMake, written by cargo xtask sysroot.
set(CMAKE_SHARED_LIBRARY_PREFIX "lib")
set(CMAKE_SHARED_LIBRARY_SUFFIX ".dlib")
set(CMAKE_SHARED_MODULE_PREFIX "lib")
set(CMAKE_SHARED_MODULE_SUFFIX ".dlib")
set(CMAKE_STATIC_LIBRARY_PREFIX "lib")
set(CMAKE_STATIC_LIBRARY_SUFFIX ".slib")
set(CMAKE_FIND_LIBRARY_PREFIXES "lib")
set(CMAKE_FIND_LIBRARY_SUFFIXES ".dlib" ".slib")
# The version goes before the suffix, which the Mikros patch of CMake
# allows: lib<name>.<version>.dlib, with the soname lib<name>.<major>.dlib.
# A shared library without a version has the major version 0.
set(CMAKE_SHARED_LIBRARY_VERSION_BEFORE_SUFFIX 1)
set(CMAKE_SHARED_LIBRARY_DEFAULT_SOVERSION 0)
set(CMAKE_SHARED_LIBRARY_SONAME_C_FLAG "-Wl,-soname,")
set(CMAKE_SHARED_LIBRARY_RUNTIME_C_FLAG "-Wl,-rpath,")
set(CMAKE_SHARED_LIBRARY_RUNTIME_C_FLAG_SEP ":")
set(CMAKE_SHARED_LIBRARY_RPATH_ORIGIN_TOKEN "\$ORIGIN")
set(CMAKE_EXE_EXPORTS_C_FLAG "-Wl,--export-dynamic")
# The C library has the dynamic loading functions.
set(CMAKE_DL_LIBS "")
include(Platform/UnixPaths)
"#;

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
    fn build_system_files_select_mikros_and_the_compilers() {
        let cross = meson_cross("../llvm/bin");
        for line in [
            "c = ['@DIRNAME@/../llvm/bin/clang', '--config=@DIRNAME@/x86_64-unknown-mikros.cfg']",
            "sys_root = '@DIRNAME@'",
            "system = 'mikros'",
        ] {
            assert!(cross.lines().any(|l| l == line), "{line}");
        }
        let toolchain = cmake_toolchain("../llvm/bin");
        for line in [
            "set(CMAKE_SYSTEM_NAME Mikros)",
            "set(CMAKE_SYSROOT \"${CMAKE_CURRENT_LIST_DIR}\")",
            "set(CMAKE_C_COMPILER \"${_MIKROS_LLVM}/clang\" \"${_MIKROS_CONFIG}\")",
        ] {
            assert!(toolchain.lines().any(|l| l == line), "{line}");
        }
        for line in [
            "set(CMAKE_SHARED_LIBRARY_SUFFIX \".dlib\")",
            "set(CMAKE_STATIC_LIBRARY_SUFFIX \".slib\")",
            "set(CMAKE_SHARED_LIBRARY_VERSION_BEFORE_SUFFIX 1)",
            "set(CMAKE_SHARED_LIBRARY_DEFAULT_SOVERSION 0)",
        ] {
            assert!(CMAKE_PLATFORM.lines().any(|l| l == line), "{line}");
        }
    }
}
