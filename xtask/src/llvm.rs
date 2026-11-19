//! The pinned LLVM release whose Clang and LLD build C and C++ code for
//! Mikros: fetching its source, applying the Mikros patches in
//! `tools/llvm/patches/`, and building and installing Clang and LLD in
//! `target/llvm/install`, with the compiler-rt builtins and the C++
//! runtimes for Mikros.

use crate::tools::Version;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The pinned LLVM release.
pub const LLVM: Version = Version(23, 1, 4);
const URL: &str = "https://github.com/llvm/llvm-project/releases/download/\
                   llvmorg-23.1.4/llvm-project-23.1.4.src.tar.xz";
/// SHA-256 of the source archive at `URL`.
const SHA256: &str = "819ecf7351f1a66f76b2d701d8942faaf5faeb324cb90c9d5b090de142434bb2";
const SOURCE: &str = "llvm-project-23.1.4.src";
/// The file in the patched source that records which patches it has.
const STAMP: &str = ".mikros-patches";
/// The CMake options of the build: Clang and LLD for x86_64 only.
const CMAKE_OPTIONS: [&str; 16] = [
    "-DCMAKE_BUILD_TYPE=Release",
    "-DCMAKE_C_COMPILER=clang",
    "-DCMAKE_CXX_COMPILER=clang++",
    "-DLLVM_USE_LINKER=lld",
    "-DLLVM_ENABLE_PROJECTS=clang;lld",
    "-DLLVM_TARGETS_TO_BUILD=X86",
    "-DLLVM_ENABLE_ASSERTIONS=OFF",
    "-DLLVM_INCLUDE_TESTS=OFF",
    "-DLLVM_INCLUDE_BENCHMARKS=OFF",
    "-DLLVM_INCLUDE_EXAMPLES=OFF",
    "-DLLVM_INCLUDE_DOCS=OFF",
    "-DLLVM_ENABLE_ZSTD=OFF",
    "-DLLVM_ENABLE_LIBXML2=OFF",
    "-DLLVM_PARALLEL_LINK_JOBS=2",
    "-DCLANG_ENABLE_ARCMT=OFF",
    "-DCLANG_ENABLE_STATIC_ANALYZER=OFF",
];
/// The installed parts of the build.
const COMPONENTS: [&str; 6] = [
    "install-clang",
    "install-clang-resource-headers",
    "install-lld",
    "install-llvm-ar",
    "install-llvm-ranlib",
    "install-llvm-objcopy",
];

fn dir() -> PathBuf {
    crate::workspace_root().join("target/llvm")
}

/// The directory of the installed programs.
pub fn bin() -> PathBuf {
    dir().join("install/bin")
}

/// The installed program `name`, such as `clang` or `ld.lld`.
pub fn tool(name: &str) -> PathBuf {
    bin().join(name)
}

/// Fetches, patches, builds and installs Clang and LLD. A build that is
/// up to date only checks the patches and runs Ninja.
pub fn build() -> Result<()> {
    let archive = dir().join(format!("{SOURCE}.tar.xz"));
    crate::fetch::cached(&format!("LLVM {LLVM} source"), URL, SHA256, &archive)?;
    let source = patched_source(&archive)?;
    let build = dir().join("build");
    let mut args = vec![
        format!("-DCMAKE_INSTALL_PREFIX={}", dir().join("install").display()),
        // The source lies in this repository, whose commit would
        // otherwise enter the version, and relink Clang at every commit.
        "-DLLVM_APPEND_VC_REV=OFF".to_owned(),
    ];
    args.extend(CMAKE_OPTIONS.iter().map(|o| o.to_string()));
    // A build configured with other options is configured again.
    let configured = build.join(".mikros-cmake-options");
    if fs::read_to_string(&configured).ok() != Some(args.join("\n")) {
        fs::create_dir_all(&build)?;
        crate::port::run(
            Command::new("cmake")
                .current_dir(&build)
                .args(["-G", "Ninja", "-S"])
                .arg(source.join("llvm"))
                .args(&args),
        )
        .context("configuring LLVM")?;
        fs::write(&configured, args.join("\n"))?;
    }
    crate::port::run(Command::new("ninja").current_dir(&build).args(COMPONENTS))
        .context("building LLVM")
}

/// The resource directory of the installed Clang.
fn resource_dir() -> PathBuf {
    dir().join(format!("install/lib/clang/{}", LLVM.0))
}

/// The compiler-rt builtins for the Mikros target, which Clang links
/// into every program and library.
pub fn builtins() -> PathBuf {
    resource_dir().join("lib/x86_64-unknown-mikros/libclang_rt.builtins.a")
}

/// Builds the compiler-rt builtins for the Mikros target with the
/// installed Clang against the C headers in `sysroot`, and installs them
/// where its driver finds them.
pub fn build_builtins(sysroot: &Path) -> Result<()> {
    let source = dir().join(SOURCE);
    let build = dir().join("builtins");
    let target = crate::sysroot::TRIPLE;
    let args = [
        format!(
            "-DLLVM_DIR={}",
            dir().join("build/lib/cmake/llvm").display()
        ),
        format!("-DCMAKE_INSTALL_PREFIX={}", resource_dir().display()),
        format!("-DCMAKE_C_COMPILER={}", tool("clang").display()),
        format!("-DCMAKE_ASM_COMPILER={}", tool("clang").display()),
        format!("-DCMAKE_AR={}", tool("llvm-ar").display()),
        format!("-DCMAKE_RANLIB={}", tool("llvm-ranlib").display()),
        format!("-DCMAKE_C_COMPILER_TARGET={target}"),
        format!("-DCMAKE_ASM_COMPILER_TARGET={target}"),
        "-DCMAKE_BUILD_TYPE=Release".to_owned(),
        "-DCMAKE_SYSTEM_NAME=Generic".to_owned(),
        // The C library is not built yet, only its headers installed.
        "-DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY".to_owned(),
        format!(
            "-DCMAKE_C_FLAGS=-march=x86-64-v3 --sysroot={}",
            sysroot.display()
        ),
        format!(
            "-DCMAKE_ASM_FLAGS=-march=x86-64-v3 --sysroot={}",
            sysroot.display()
        ),
        "-DCOMPILER_RT_DEFAULT_TARGET_ONLY=ON".to_owned(),
        "-DCOMPILER_RT_BAREMETAL_BUILD=ON".to_owned(),
        "-DLLVM_ENABLE_PER_TARGET_RUNTIME_DIR=ON".to_owned(),
    ];
    let configured = build.join(".mikros-cmake-options");
    if fs::read_to_string(&configured).ok() != Some(args.join("\n")) {
        fs::create_dir_all(&build)?;
        crate::port::run(
            Command::new("cmake")
                .args(["-G", "Ninja", "-S"])
                .arg(source.join("compiler-rt/lib/builtins"))
                .arg("-B")
                .arg(&build)
                .args(&args),
        )
        .context("configuring the compiler-rt builtins")?;
        fs::write(&configured, args.join("\n"))?;
    }
    crate::port::run(Command::new("ninja").current_dir(&build).arg("install"))
        .context("building the compiler-rt builtins")
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

/// Builds LLVM's C++ runtimes for Mikros with the CMake that
/// `crate::cmake` builds and the sysroot's toolchain file `toolchain`,
/// which names their shared libraries `libunwind.dlib`, `libc++abi.dlib`
/// and `libc++.dlib` with those sonames, and installs them in the
/// sysroot `sysroot`: the headers in `include/c++/v1` and the libraries,
/// shared and static, in `lib`.
pub fn build_cxx_runtimes(toolchain: &Path, sysroot: &Path) -> Result<()> {
    let cmake = crate::cmake::build()?;
    let build = dir().join("cxx");
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
                .arg(dir().join(SOURCE).join("runtimes"))
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

/// The source with every patch applied, extracted again from `archive`
/// whenever the patches changed. Extraction restores the archive's file
/// times, so the files that the old or the new patches touch are marked
/// as changed for Ninja to rebuild what depends on them.
fn patched_source(archive: &Path) -> Result<PathBuf> {
    let patches = crate::port::patches(&crate::workspace_root().join("tools/llvm/patches"))?;
    let stamp = crate::port::stamp(&patches)?;
    let source = dir().join(SOURCE);
    let applied = fs::read_to_string(source.join(STAMP)).ok();
    if applied.as_deref() == Some(stamp.as_str()) {
        return Ok(source);
    }
    let mut touched = fs::read_to_string(dir().join("patched-files"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect::<Vec<_>>();
    if source.exists() {
        fs::remove_dir_all(&source).with_context(|| format!("removing {}", source.display()))?;
    }
    println!("extracting {}", archive.display());
    crate::port::run(
        Command::new("tar")
            .current_dir(dir())
            .arg("-xJf")
            .arg(archive),
    )
    .context("extracting the LLVM source")?;
    for patch in &patches {
        crate::port::run(
            Command::new("patch")
                .args(["-p1", "-s", "-d"])
                .arg(&source)
                .arg("-i")
                .arg(patch),
        )
        .with_context(|| format!("applying {}", patch.display()))?;
        touched.extend(patched_files(&fs::read_to_string(patch)?));
    }
    touched.sort();
    touched.dedup();
    for file in &touched {
        let path = source.join(file);
        if path.exists() {
            crate::port::run(Command::new("touch").arg(&path))?;
        }
    }
    fs::write(dir().join("patched-files"), touched.join("\n"))?;
    fs::write(source.join(STAMP), stamp)?;
    Ok(source)
}

/// The paths, relative to the source, of the files that `patch` changes.
fn patched_files(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|l| l.strip_prefix("+++ b/"))
        .map(|p| p.split('\t').next().unwrap_or(p).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_patched_files() {
        let patch = "diff -ruN a/x.h b/x.h\n--- a/x.h\n+++ b/x.h\n@@ -1 +1 @@\n\
                     -a\n+b\n--- a/d/y.cpp\t2026\n+++ b/d/y.cpp\t2026\n";
        assert_eq!(patched_files(patch), ["x.h", "d/y.cpp"]);
    }
}
