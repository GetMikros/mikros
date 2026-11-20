//! LLVM for Mikros, the port in `user/ports/llvm`: the C++ runtimes
//! libunwind, libc++abi and libc++, and LLVM's libraries as
//! `libLLVM.dlib`, built from the release of the toolchain
//! (`crate::llvm`), whose archive it shares, with the port's own patches.

use crate::ports::Port;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The directories of the release that the port builds from.
const PARTS: [&str; 8] = [
    "cmake",
    "libc",
    "libcxx",
    "libcxxabi",
    "libunwind",
    "llvm",
    "runtimes",
    "third-party",
];

/// Their tests, which the port does not build.
const LEFT_OUT: [&str; 5] = [
    "libc/test",
    "libcxx/test",
    "libcxxabi/test",
    "libunwind/test",
    "llvm/test",
];

/// The port, which must pin the toolchain's release: the C++ runtimes
/// and the compiler that builds against them are one release.
fn port() -> Result<Port> {
    let port = crate::ports::load("llvm")?;
    check_pin(&port)?;
    Ok(port)
}

fn check_pin(port: &Port) -> Result<()> {
    let pin = (
        port.url.as_str(),
        port.sha256.as_str(),
        port.source.as_str(),
    );
    if pin != (crate::llvm::URL, crate::llvm::SHA256, crate::llvm::SOURCE) {
        bail!(
            "user/ports/llvm/port.toml pins another release than LLVM {} of xtask/src/llvm.rs",
            crate::llvm::LLVM
        );
    }
    Ok(())
}

/// The port's patched source, in `target/sources/llvm`.
fn source() -> Result<PathBuf> {
    let port = port()?;
    let dir = crate::workspace_root().join("target/sources/llvm");
    crate::llvm::patched_source(
        &crate::llvm::archive()?,
        &dir,
        &port.dir.join("patches"),
        &PARTS,
        &LEFT_OUT,
    )
}

/// The CMake options of the C++ runtimes for Mikros: libunwind,
/// libc++abi and libc++, each shared and static, the shared ones linking
/// each other and libunwind.dlib rather than holding them, so that a
/// process has one unwinder, on the C library's threads, files and time.
const CXX_RUNTIME_OPTIONS: [&str; 35] = [
    "-DLLVM_ENABLE_RUNTIMES=libunwind;libcxxabi;libcxx",
    "-DCMAKE_BUILD_TYPE=Release",
    // musl declares `Dl_info`, which libunwind uses, only for _GNU_SOURCE.
    "-DCMAKE_C_FLAGS=-D_GNU_SOURCE",
    "-DCMAKE_CXX_FLAGS=-D_GNU_SOURCE",
    "-DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY",
    "-DLLVM_INCLUDE_TESTS=OFF",
    "-DLIBUNWIND_ENABLE_SHARED=ON",
    "-DLIBUNWIND_ENABLE_STATIC=ON",
    "-DLIBUNWIND_USE_COMPILER_RT=ON",
    "-DLIBUNWIND_INCLUDE_TESTS=OFF",
    "-DLIBUNWIND_HAS_DL_LIB=OFF",
    "-DLIBUNWIND_HAS_PTHREAD_LIB=OFF",
    "-DLIBCXXABI_USE_LLVM_UNWINDER=ON",
    "-DLIBCXXABI_USE_COMPILER_RT=ON",
    "-DLIBCXXABI_ENABLE_SHARED=ON",
    "-DLIBCXXABI_ENABLE_STATIC=ON",
    "-DLIBCXXABI_STATICALLY_LINK_UNWINDER_IN_SHARED_LIBRARY=OFF",
    "-DLIBCXXABI_STATICALLY_LINK_UNWINDER_IN_STATIC_LIBRARY=OFF",
    "-DLIBCXXABI_INCLUDE_TESTS=OFF",
    "-DLIBCXX_CXX_ABI=libcxxabi",
    "-DLIBCXX_HAS_MUSL_LIBC=ON",
    "-DLIBCXX_USE_COMPILER_RT=ON",
    "-DLIBCXX_ENABLE_SHARED=ON",
    "-DLIBCXX_ENABLE_STATIC=ON",
    "-DLIBCXX_STATICALLY_LINK_ABI_IN_SHARED_LIBRARY=OFF",
    "-DLIBCXX_STATICALLY_LINK_ABI_IN_STATIC_LIBRARY=OFF",
    "-DLIBCXX_INCLUDE_TESTS=OFF",
    "-DLIBCXX_INCLUDE_BENCHMARKS=OFF",
    "-DLIBCXX_INSTALL_MODULES=OFF",
    // The driver links libc++abi and libunwind itself, and a linker script
    // would take the library's own name.
    "-DLIBCXX_ENABLE_ABI_LINKER_SCRIPT=OFF",
    // The C library has them, and libc++ does not tell its threads from
    // the target's name. The checks for libraries of their own would
    // find them, since nothing is linked when they run.
    "-DLIBCXX_HAS_PTHREAD_LIB=OFF",
    "-DLIBCXX_HAS_RT_LIB=OFF",
    "-DLIBCXX_HAS_ATOMIC_LIB=OFF",
    "-DLIBCXXABI_HAS_PTHREAD_LIB=OFF",
    "-DLIBCXX_HAS_PTHREAD_API=ON",
];

/// Builds LLVM's C++ runtimes for Mikros from the port's source with the
/// CMake that `crate::cmake` builds and the sysroot's toolchain file
/// `toolchain`, which names their shared libraries `libunwind.dlib`,
/// `libc++abi.dlib` and `libc++.dlib` with those sonames, and installs
/// them in the sysroot `sysroot`: the headers in `include/c++/v1` and the
/// libraries, shared and static, in `lib`.
pub fn build_cxx_runtimes(toolchain: &Path, sysroot: &Path) -> Result<()> {
    let source = source()?;
    let cmake = crate::cmake::build()?;
    let build = crate::workspace_root().join("target/ported/llvm-runtimes");
    let mut args: Vec<String> = CXX_RUNTIME_OPTIONS.iter().map(|s| s.to_string()).collect();
    args.push(format!("-DCMAKE_TOOLCHAIN_FILE={}", toolchain.display()));
    args.push(format!("-DCMAKE_INSTALL_PREFIX={}", sysroot.display()));
    args.push("-DCMAKE_INSTALL_LIBDIR=lib".to_owned());
    // Configured again when the options, the toolchain, the CMake patches
    // or the C library's soname change.
    let stamp = format!(
        "{}\n{}\n{}\n{}",
        args.join("\n"),
        fs::read_to_string(toolchain).unwrap_or_default(),
        crate::cmake::patches_stamp(),
        crate::musl::SONAME,
    );
    let configured = build.join(".mikros-cmake-options");
    if fs::read_to_string(&configured).ok() != Some(stamp.clone()) {
        if build.exists() {
            fs::remove_dir_all(&build)?;
        }
        fs::create_dir_all(&build)?;
        crate::port::run(
            Command::new(&cmake)
                .args(["-G", "Ninja", "-S"])
                .arg(source.join("runtimes"))
                .arg("-B")
                .arg(&build)
                .args(&args),
        )
        .context("configuring the C++ runtimes")?;
        fs::write(&configured, stamp)?;
    }
    // The driver configuration names the installed headers for C++, and
    // libc++'s headers would find them as the next of their own names, so
    // they go while the runtimes are built, and are installed again.
    let headers = sysroot.join("include/c++");
    if headers.exists() {
        fs::remove_dir_all(&headers).with_context(|| format!("removing {}", headers.display()))?;
    }
    crate::port::run(
        Command::new("ninja")
            .current_dir(&build)
            .args(["--quiet", "install"]),
    )
    .context("building the C++ runtimes")
}

/// The CMake options of LLVM's libraries for Mikros: the X86 target
/// only, all of them in one shared library, `libLLVM.dlib`, without
/// programs, tests or optional libraries.
const LIBRARY_OPTIONS: [&str; 24] = [
    "-DLLVM_TARGETS_TO_BUILD=X86",
    "-DLLVM_HOST_TRIPLE=x86_64-unknown-mikros",
    "-DLLVM_DEFAULT_TARGET_TRIPLE=x86_64-unknown-mikros",
    "-DLLVM_BUILD_LLVM_DYLIB=ON",
    "-DLLVM_LINK_LLVM_DYLIB=ON",
    "-DLLVM_BUILD_TOOLS=OFF",
    "-DLLVM_BUILD_UTILS=OFF",
    "-DLLVM_TOOL_LTO_BUILD=OFF",
    "-DLLVM_TOOL_REMARKS_SHLIB_BUILD=OFF",
    "-DLLVM_INCLUDE_TESTS=OFF",
    "-DLLVM_INCLUDE_EXAMPLES=OFF",
    "-DLLVM_INCLUDE_BENCHMARKS=OFF",
    "-DLLVM_INCLUDE_DOCS=OFF",
    "-DLLVM_ENABLE_BINDINGS=OFF",
    "-DLLVM_ENABLE_ASSERTIONS=OFF",
    "-DLLVM_ENABLE_PIC=ON",
    "-DLLVM_ENABLE_THREADS=ON",
    "-DLLVM_ENABLE_ZLIB=OFF",
    "-DLLVM_ENABLE_ZSTD=OFF",
    "-DLLVM_ENABLE_LIBXML2=OFF",
    "-DLLVM_ENABLE_LIBEDIT=OFF",
    "-DLLVM_ENABLE_LIBPFM=OFF",
    "-DLLVM_ENABLE_CURL=OFF",
    "-DLLVM_ENABLE_HTTPLIB=OFF",
];

/// Builds LLVM's libraries for Mikros from the port's source, running the
/// toolchain's table generators, and returns the directory its prefix,
/// `/system`, was installed in: `lib/libLLVM.dlib`, with its headers and
/// CMake package. LLVM installs its libraries in `lib/` whatever
/// `CMAKE_INSTALL_LIBDIR` says.
pub fn build_library() -> Result<PathBuf> {
    crate::llvm::build()?;
    let source = source()?;
    let mut options: Vec<String> = LIBRARY_OPTIONS.iter().map(|o| o.to_string()).collect();
    options.push(format!(
        "-DLLVM_NATIVE_TOOL_DIR={}",
        crate::llvm::native_tools().display()
    ));
    let install = crate::build_systems::cmake("llvm", &source.join("llvm"), &options)?;
    crate::build_systems::check_installed(
        &install,
        &[
            "lib/libLLVM.dlib",
            "include/llvm/IR/Module.h",
            "lib/cmake/llvm/LLVMConfig.cmake",
        ],
        &[],
    )?;
    let library = install.join("lib/libLLVM.dlib");
    let soname = crate::build_systems::soname(&library)?;
    if soname.as_deref() != Some("libLLVM.dlib") {
        bail!(
            "{} has the soname {soname:?}, not libLLVM.dlib",
            library.display()
        );
    }
    Ok(install)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_the_toolchains_release() {
        let port = crate::ports::load("llvm").unwrap();
        check_pin(&port).unwrap();
        let other = Port {
            sha256: "0".repeat(64),
            ..port
        };
        assert!(check_pin(&other).is_err());
    }
}
