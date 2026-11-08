//! Linux's btrfs code on the btrfs shim (MIKSPEC-0053.R1): the pinned
//! Linux release, fetched once and extracted in the parts the shim
//! builds from, and compiled, for Mikros or for the build host, against
//! the Mikros architecture in `user/btrfs/linux` into a static library.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The pinned release, by the SHA-256 of its archive.
const RELEASE: &str = "linux-6.18.62";
const URL: &str = "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.18.62.tar.gz";
const SHA256: &str = "b3db543d71c7334292b37b9da58fbd10339f558ad4b7aaa0ebc4e5c18279534c";

/// The parts of the release that the shim builds from, as prefixes of
/// their paths.
const PARTS: &[&str] = &[
    "COPYING",
    "LICENSES/",
    "include/",
    "kernel/bounds.c",
    "fs/btrfs/",
    "lib/",
    "crypto/",
];

#[derive(clap::Args)]
pub struct Args {
    /// Build for the build host rather than for Mikros.
    #[arg(long)]
    pub host: bool,
}

/// What the library is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Mikros,
    Host,
}

impl Flavor {
    fn name(self) -> &'static str {
        match self {
            Flavor::Mikros => "mikros",
            Flavor::Host => "host",
        }
    }

    fn target(self) -> &'static [&'static str] {
        match self {
            Flavor::Mikros => &["--target=x86_64-unknown-mikros", "-march=x86-64-v3"],
            Flavor::Host => &["--target=x86_64-unknown-linux-gnu"],
        }
    }
}

pub fn run(args: &Args) -> Result<()> {
    let flavor = if args.host {
        Flavor::Host
    } else {
        Flavor::Mikros
    };
    let library = build(flavor)?;
    println!("built    {}", library.display());
    Ok(())
}

/// The directory of the release's extracted parts, fetching and
/// extracting them unless they already are.
pub fn source() -> Result<PathBuf> {
    let dir = crate::workspace_root().join("target/sources/linux");
    let archive = dir.join(format!("{RELEASE}.tar.gz"));
    crate::fetch::cached(&format!("{RELEASE} source"), URL, SHA256, &archive)?;
    let source = dir.join(RELEASE);
    let stamp = source.join(".mikros-parts");
    let parts = PARTS.join("\n");
    if fs::read_to_string(&stamp).ok().as_deref() == Some(parts.as_str()) {
        return Ok(source);
    }
    if source.exists() {
        fs::remove_dir_all(&source).with_context(|| format!("removing {}", source.display()))?;
    }
    println!("extracting {} from {}", PARTS.join(" "), archive.display());
    let gz = flate2::read::GzDecoder::new(File::open(&archive)?);
    let mut tar = tar::Archive::new(gz);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let Ok(inside) = path.strip_prefix(RELEASE) else {
            continue;
        };
        let inside = inside.to_string_lossy();
        if PARTS.iter().any(|part| inside.starts_with(part)) {
            entry
                .unpack_in(&dir)
                .with_context(|| format!("extracting {}", path.display()))?;
        }
    }
    fs::write(&stamp, parts)?;
    Ok(source)
}

/// Builds the library for `flavor` unless it is up to date, and returns
/// its path.
pub fn build(flavor: Flavor) -> Result<PathBuf> {
    let source = source()?;
    let linux = crate::workspace_root().join("user/btrfs/linux");
    let out = crate::workspace_root()
        .join("target/btrfs")
        .join(flavor.name());
    let library = out.join("libbtrfs.a");
    let files = btrfs_files(&source)?;
    let flags = flags(flavor, &source, &linux, &out);
    let stamp = digest(&flags, &files, &linux)?;
    let stamp_file = out.join(".stamp");
    if library.exists() && fs::read_to_string(&stamp_file).ok().as_deref() == Some(&stamp) {
        return Ok(library);
    }
    if out.exists() {
        fs::remove_dir_all(&out).with_context(|| format!("removing {}", out.display()))?;
    }
    wrappers(&source, &linux, &out)?;
    offsets(
        &source,
        &flags,
        "kernel/bounds.c",
        &out.join("include/generated/bounds.h"),
    )?;
    let objects = compile(&source, &flags, &files, &out.join("obj"))?;
    let mut ar = Command::new(crate::llvm::tool("llvm-ar"));
    ar.arg("rcs").arg(&library).args(&objects);
    crate::port::run(&mut ar)?;
    fs::write(&stamp_file, stamp)?;
    Ok(library)
}

/// The files of btrfs that the configuration builds: its Makefile's
/// `btrfs-y`, as sources relative to the tree.
fn btrfs_files(source: &Path) -> Result<Vec<String>> {
    let files = btrfs_y(&fs::read_to_string(source.join("fs/btrfs/Makefile"))?);
    if files.is_empty() {
        bail!("found no btrfs-y in fs/btrfs/Makefile");
    }
    Ok(files)
}

/// The sources of the objects that the `btrfs-y` lines of `makefile`
/// name, up to the blank line after them.
fn btrfs_y(makefile: &str) -> Vec<String> {
    let mut files = Vec::new();
    let mut inside = false;
    for line in makefile.lines() {
        if line.starts_with("btrfs-y") {
            inside = true;
        } else if line.trim().is_empty() {
            inside = false;
        }
        if !inside {
            continue;
        }
        for word in line.split_whitespace() {
            if let Some(name) = word.strip_suffix(".o") {
                files.push(format!("fs/btrfs/{name}.c"));
            }
        }
    }
    files
}

/// The compiler's flags for every file: the target, the include paths
/// in Kbuild's order with the Mikros architecture's in place of an
/// architecture of the tree, the headers Kbuild includes first, and the
/// language options the kernel's code is written for.
fn flags(flavor: Flavor, source: &Path, linux: &Path, out: &Path) -> Vec<String> {
    let arch = linux.join("arch/mikros/include");
    let generated = out.join("arch/include/generated");
    let mut flags: Vec<String> = flavor.target().iter().map(|s| s.to_string()).collect();
    let path = |p: &Path| p.display().to_string();
    for dir in [
        out.join("include"),
        linux.join("include"),
        arch.clone(),
        generated.clone(),
        source.join("include"),
        arch.join("uapi"),
        generated.join("uapi"),
        source.join("include/uapi"),
    ] {
        flags.push(format!("-I{}", path(&dir)));
    }
    for header in [
        "linux/compiler-version.h",
        "linux/kconfig.h",
        "linux/compiler_types.h",
    ] {
        flags.push("-include".into());
        flags.push(path(&source.join("include").join(header)));
    }
    flags.extend(
        [
            "-fPIE",
            "-nostdinc",
            "-ffreestanding",
            "-fno-stack-protector",
            "-D__KERNEL__",
            "-DKBUILD_MODNAME=\"btrfs\"",
            "-DKBUILD_MODFILE=\"fs/btrfs/btrfs\"",
            "-std=gnu11",
            "-fshort-wchar",
            "-funsigned-char",
            "-fno-common",
            "-fno-strict-aliasing",
            "-fno-delete-null-pointer-checks",
            "-fno-strict-overflow",
            "-fno-builtin-wcslen",
            "-O2",
            "-g",
            "-Wall",
            "-Wundef",
            "-Werror",
            "-Wno-format-security",
            "-Wno-trigraphs",
            "-Wno-address-of-packed-member",
            "-Wno-gnu-variable-sized-type-not-at-end",
            "-Wno-pointer-sign",
            "-Wno-unused-but-set-variable",
            "-Wno-initializer-overrides",
            "-Wno-format-zero-length",
        ]
        .map(String::from),
    );
    flags
}

/// A digest of everything a build depends on that the source's own
/// stamp does not cover: the flags, the files, and the Mikros
/// architecture's files.
fn digest(flags: &[String], files: &[String], linux: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    for item in flags.iter().chain(files) {
        hasher.update(item.as_bytes());
        hasher.update([0]);
    }
    let mut paths = Vec::new();
    collect(linux, &mut paths)?;
    paths.sort();
    for path in paths {
        hasher.update(path.as_os_str().as_encoded_bytes());
        hasher.update([0]);
        hasher.update(fs::read(&path)?);
        hasher.update([0]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn collect(dir: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, paths)?;
        } else {
            paths.push(path);
        }
    }
    Ok(())
}

/// The `asm/` headers that Kbuild would generate for those the Mikros
/// architecture does not provide: one including its `asm-generic/`
/// version for each `mandatory-y` of the tree.
fn wrappers(source: &Path, linux: &Path, out: &Path) -> Result<()> {
    let arch = linux.join("arch/mikros/include");
    for (kbuild, own, generated) in [
        ("include/asm-generic/Kbuild", arch.join("asm"), "asm"),
        (
            "include/uapi/asm-generic/Kbuild",
            arch.join("uapi/asm"),
            "uapi/asm",
        ),
    ] {
        let dir = out.join("arch/include/generated").join(generated);
        fs::create_dir_all(&dir)?;
        let kbuild = fs::read_to_string(source.join(kbuild))?;
        for line in kbuild.lines() {
            let Some(name) = line.strip_prefix("mandatory-y += ") else {
                continue;
            };
            let name = name.trim();
            if !own.join(name).exists() {
                fs::write(dir.join(name), format!("#include <asm-generic/{name}>\n"))?;
            }
        }
    }
    Ok(())
}

/// Writes the header `out` from the `DEFINE()`s of `file`, as Kbuild's
/// `filechk_offsets` does: compiled to assembly, whose `->NAME VALUE`
/// markers become `#define`s.
fn offsets(source: &Path, flags: &[String], file: &str, out: &Path) -> Result<()> {
    let mut clang = Command::new(crate::llvm::tool("clang"));
    clang
        .current_dir(source)
        .args(flags)
        .args(["-S", "-o", "-"])
        .arg(file);
    let assembly = crate::port::output(&mut clang)?;
    let header = format!(
        "/* Generated by xtask from the tree's {file}. */\n#pragma once\n{}",
        defines(&assembly)
    );
    fs::create_dir_all(out.parent().unwrap())?;
    fs::write(out, header)?;
    Ok(())
}

/// The `#define`s of the `->NAME VALUE` markers in `assembly`.
fn defines(assembly: &str) -> String {
    let mut out = String::new();
    for line in assembly.lines() {
        let Some(rest) = line.trim_start().strip_prefix(".ascii") else {
            continue;
        };
        let Some(marker) = rest.trim().trim_matches('"').strip_prefix("->") else {
            continue;
        };
        let mut words = marker.split_whitespace();
        let (Some(name), Some(value)) = (words.next(), words.next()) else {
            continue;
        };
        let value = value.trim_start_matches(['$', '#']);
        out.push_str(&format!("#define {name} {value}\n"));
    }
    out
}

/// Compiles `files` of the tree into objects in `dir`, as many at once
/// as the machine has threads, and returns their paths.
fn compile(source: &Path, flags: &[String], files: &[String], dir: &Path) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(dir)?;
    let objects: Vec<PathBuf> = files
        .iter()
        .map(|f| dir.join(f.replace('/', "_")).with_extension("o"))
        .collect();
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(file) = files.get(i) else {
                        return;
                    };
                    let base = Path::new(file).file_stem().unwrap().to_string_lossy();
                    let output = Command::new(crate::llvm::tool("clang"))
                        .current_dir(source)
                        .args(flags)
                        .arg(format!("-DKBUILD_BASENAME=\"{base}\""))
                        .arg("-c")
                        .arg(file)
                        .arg("-o")
                        .arg(&objects[i])
                        .output();
                    let failed = match output {
                        Ok(o) if o.status.success() => None,
                        Ok(o) => Some(format!("{file}:\n{}", String::from_utf8_lossy(&o.stderr))),
                        Err(e) => Some(format!("{file}: {e}")),
                    };
                    if let Some(failed) = failed {
                        failures.lock().unwrap().push(failed);
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    if !failures.is_empty() {
        bail!(
            "{} of {} files failed to compile:\n{}",
            failures.len(),
            files.len(),
            failures.join("\n")
        );
    }
    Ok(objects)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_objects_of_btrfs_y() {
        let makefile = "obj-$(CONFIG_BTRFS_FS) := btrfs.o\n\n\
                        btrfs-y += super.o ctree.o \\\n\
                        \t   ulist.o\n\
                        \n\
                        btrfs-$(CONFIG_BTRFS_FS_POSIX_ACL) += acl.o\n";
        assert_eq!(
            btrfs_y(makefile),
            ["fs/btrfs/super.c", "fs/btrfs/ctree.c", "fs/btrfs/ulist.c"]
        );
    }

    #[test]
    fn defines_the_markers_of_the_assembly() {
        let assembly = "main:\n\
                        \t.ascii\t\"->NR_PAGEFLAGS $20 __NR_PAGEFLAGS\"\n\
                        \t.ascii\t\"->\"\n\
                        \t.ascii \"->SPINLOCK_SIZE $4 sizeof(spinlock_t)\"\n\
                        \tret\n";
        assert_eq!(
            defines(assembly),
            "#define NR_PAGEFLAGS 20\n#define SPINLOCK_SIZE 4\n"
        );
    }
}
