//! ACPICA, which the ACPI server runs on: the pinned release, compiled
//! with xtask's LLVM into a static library, `target/acpica/<flavor>/
//! libacpica.a`, for Mikros or for the build host's tests.
//!
//! The release's components are compiled freestanding, with ACPICA's own
//! C library routines, and with the release's headers copied and given a
//! platform header for Mikros, `user/drivers/acpi/platform/acmikros.h`, as is
//! the ACPI server's glue, `user/drivers/acpi/glue`, which gives what the server
//! asks of ACPICA simple types; a change to any of these rebuilds the
//! library. The operating system layer, the `AcpiOs`
//! functions, is the program's that links the library. The objects are
//! linked into one, whose only global symbols are ACPICA's `Acpi` ones and
//! the glue's `mikros_acpi` ones, so that ACPICA's own `memcpy` and the
//! like stay its own.

use anyhow::{Context, Result, bail};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The components of the release that the server needs: all but the
/// debugger and the disassembler.
const COMPONENTS: [&str; 9] = [
    "dispatcher",
    "events",
    "executer",
    "hardware",
    "namespace",
    "parser",
    "resources",
    "tables",
    "utilities",
];
/// Files of those that only the debugger and the disassembler use.
const LEFT_OUT: [&str; 1] = ["rsdump.c"];
/// The line of the release's `acenv.h` that stops a build for a platform
/// it does not know, and what takes its place.
const UNKNOWN_PLATFORM: &str = "#error Unknown target environment";
const MIKROS_PLATFORM: &str = "#include \"acmikros.h\"";

#[derive(clap::Args)]
pub struct Args {
    /// Build for the build host rather than for Mikros.
    #[arg(long)]
    pub host: bool,
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

/// The directory of the extracted release, fetching and extracting it
/// unless it already is.
fn source() -> Result<PathBuf> {
    let dir = crate::workspace_root().join("target/sources/acpica");
    let port = crate::ports::load("acpica")?;
    let archive = dir.join(format!("{}.tar.gz", port.source));
    crate::fetch::cached(
        &format!("{} source", port.source),
        &port.url,
        &port.sha256,
        &archive,
    )?;
    let source = dir.join(&port.source);
    let stamp = source.join(".mikros-extracted");
    if stamp.exists() {
        return Ok(source);
    }
    if source.exists() {
        fs::remove_dir_all(&source).with_context(|| format!("removing {}", source.display()))?;
    }
    println!("extracting {}", archive.display());
    let gz = flate2::read::GzDecoder::new(File::open(&archive)?);
    tar::Archive::new(gz)
        .unpack(&dir)
        .with_context(|| format!("extracting {}", archive.display()))?;
    File::create(&stamp)?;
    Ok(source)
}

/// The release's `acenv.h` with Mikros's platform header in place of the
/// line that stops a build for an unknown platform.
fn with_mikros_platform(acenv: &str) -> Result<String> {
    if acenv.matches(UNKNOWN_PLATFORM).count() != 1 {
        bail!("acenv.h does not stop once for an unknown platform");
    }
    Ok(acenv.replace(UNKNOWN_PLATFORM, MIKROS_PLATFORM))
}

/// Copies the files of `from` into `to`, with their subdirectories.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let path = entry?.path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            fs::copy(&path, &target).with_context(|| format!("copying {}", path.display()))?;
        }
    }
    Ok(())
}

/// The release's C files that the library is built from, relative to its
/// `source/components`.
fn files(components: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for component in COMPONENTS {
        for entry in fs::read_dir(components.join(component))? {
            let path = entry?.path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.ends_with(".c") && !LEFT_OUT.contains(&name.as_str()) {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Builds the library for `flavor` and returns its path.
pub fn build(flavor: Flavor) -> Result<PathBuf> {
    crate::llvm::build()?;
    let source = source()?;
    let out = crate::workspace_root()
        .join("target/acpica")
        .join(flavor.name());
    let library = out.join("libacpica.a");
    let platform = crate::workspace_root().join("user/drivers/acpi/platform/acmikros.h");
    let mikros =
        fs::read_to_string(&platform).with_context(|| format!("reading {}", platform.display()))?;
    let glue_dir = crate::workspace_root().join("user/drivers/acpi/glue");
    let mut glue_files = Vec::new();
    for entry in fs::read_dir(&glue_dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "c" || e == "h") {
            glue_files.push(path);
        }
    }
    glue_files.sort();
    // The glue's headers change the stamp, and its C files are compiled.
    let release = crate::ports::load("acpica")?.source;
    let mut stamp = format!("{release}\n{mikros}");
    for file in &glue_files {
        stamp.push_str(&fs::read_to_string(file)?);
    }
    let glue: Vec<PathBuf> = glue_files
        .into_iter()
        .filter(|f| f.extension().is_some_and(|e| e == "c"))
        .collect();
    let stamp_file = out.join(".stamp");
    if library.exists() && fs::read_to_string(&stamp_file).ok().as_deref() == Some(&stamp) {
        return Ok(library);
    }
    if out.exists() {
        fs::remove_dir_all(&out).with_context(|| format!("removing {}", out.display()))?;
    }
    let include = out.join("include");
    copy_tree(&source.join("source/include"), &include)?;
    let acenv = include.join("platform/acenv.h");
    fs::write(&acenv, with_mikros_platform(&fs::read_to_string(&acenv)?)?)?;
    fs::write(include.join("platform/acmikros.h"), mikros)?;

    let objects_dir = out.join("obj");
    fs::create_dir_all(&objects_dir)?;
    let mut objects = Vec::new();
    let release_files = files(&source.join("source/components"))?;
    for file in release_files.iter().chain(&glue) {
        let object = objects_dir
            .join(file.file_name().unwrap())
            .with_extension("o");
        let mut clang = Command::new(crate::llvm::tool("clang"));
        clang
            .args(flavor.target())
            .args(["-ffreestanding", "-fno-builtin", "-fno-stack-protector"])
            .args(["-fPIE", "-O2", "-g", "-Wno-unused-parameter"])
            .arg("-I")
            .arg(&include)
            .arg("-c")
            .arg(file)
            .arg("-o")
            .arg(&object);
        crate::port::run(&mut clang)?;
        objects.push(object);
    }
    let object = out.join("acpica.o");
    let mut ld = Command::new(crate::llvm::tool("ld.lld"));
    ld.arg("-r").arg("-o").arg(&object).args(&objects);
    crate::port::run(&mut ld)?;
    let mut localize = Command::new(crate::llvm::tool("llvm-objcopy"));
    localize
        .args(["--wildcard", "--keep-global-symbol=Acpi*"])
        .arg("--keep-global-symbol=mikros_acpi_*")
        .arg(&object);
    crate::port::run(&mut localize)?;
    let mut ar = Command::new(crate::llvm::tool("llvm-ar"));
    ar.arg("rcs").arg(&library).arg(&object);
    crate::port::run(&mut ar)?;
    fs::write(&stamp_file, stamp)?;
    Ok(library)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn puts_the_mikros_platform_where_an_unknown_one_stops() {
        let acenv = "#elif defined(__ZEPHYR__)\n#include \"aczephyr.h\"\n#else\n\n#error Unknown target environment\n#endif\n";
        let patched = with_mikros_platform(acenv).unwrap();
        assert!(patched.contains("#else\n\n#include \"acmikros.h\"\n#endif"));
        assert!(!patched.contains("#error"));
        assert!(with_mikros_platform("#endif\n").is_err());
    }
}
