//! Linux's btrfs code on the btrfs shim (MIKSPEC-0053.R1): the pinned
//! Linux release, fetched once and extracted in the parts the shim
//! builds from, and compiled, for Mikros or for the build host, against
//! the Mikros architecture in `user/btrfs/linux` into a static library,
//! with the library code of the release that btrfs uses and the shim's
//! own files in `user/btrfs/shim`.

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
    "init/",
    "kernel/",
    "mm/",
    "block/",
    "fs/",
    "drivers/base/map.c",
    "lib/",
    "crypto/",
    "security/",
];

/// The files of the release, beside btrfs's own, that the shim builds
/// as they are: library code, locking, the VFS, the page cache and the
/// generic block code, credentials, and the crypto API with the hashes
/// btrfs checksums with.
const LINUX: &[&str] = &[
    "lib/bitmap.c",
    "lib/bitrev.c",
    "lib/bsearch.c",
    "lib/cmdline.c",
    "lib/crc/crc32-main.c",
    "lib/crypto/sha256.c",
    "lib/crypto/utils.c",
    "lib/ctype.c",
    "lib/errseq.c",
    "lib/find_bit.c",
    "lib/flex_proportions.c",
    "lib/hexdump.c",
    "lib/hweight.c",
    "lib/idr.c",
    "lib/iov_iter.c",
    "lib/kasprintf.c",
    "lib/kobject.c",
    "lib/kstrtox.c",
    "lib/list_sort.c",
    "lib/lockref.c",
    "lib/lzo/lzo1x_compress.c",
    "lib/lzo/lzo1x_compress_safe.c",
    "lib/lzo/lzo1x_decompress_safe.c",
    "lib/maple_tree.c",
    "lib/percpu_counter.c",
    "lib/radix-tree.c",
    "lib/ratelimit.c",
    "lib/rbtree.c",
    "lib/rcuref.c",
    "lib/refcount.c",
    "lib/siphash.c",
    "lib/sort.c",
    "lib/string.c",
    "lib/string_helpers.c",
    "lib/strncpy_from_user.c",
    "lib/strnlen_user.c",
    "lib/usercopy.c",
    "lib/uuid.c",
    "lib/vsprintf.c",
    "lib/xarray.c",
    "lib/xxhash.c",
    "lib/zlib_deflate/deflate.c",
    "lib/zlib_deflate/deflate_syms.c",
    "lib/zlib_deflate/deftree.c",
    "lib/zlib_inflate/inffast.c",
    "lib/zlib_inflate/inflate.c",
    "lib/zlib_inflate/inflate_syms.c",
    "lib/zlib_inflate/inftrees.c",
    "lib/zlib_inflate/infutil.c",
    "lib/zstd/common/debug.c",
    "lib/zstd/common/entropy_common.c",
    "lib/zstd/common/error_private.c",
    "lib/zstd/common/fse_decompress.c",
    "lib/zstd/common/zstd_common.c",
    "lib/zstd/compress/fse_compress.c",
    "lib/zstd/compress/hist.c",
    "lib/zstd/compress/huf_compress.c",
    "lib/zstd/compress/zstd_compress.c",
    "lib/zstd/compress/zstd_compress_literals.c",
    "lib/zstd/compress/zstd_compress_sequences.c",
    "lib/zstd/compress/zstd_compress_superblock.c",
    "lib/zstd/compress/zstd_double_fast.c",
    "lib/zstd/compress/zstd_fast.c",
    "lib/zstd/compress/zstd_lazy.c",
    "lib/zstd/compress/zstd_ldm.c",
    "lib/zstd/compress/zstd_opt.c",
    "lib/zstd/compress/zstd_preSplit.c",
    "lib/zstd/decompress/huf_decompress.c",
    "lib/zstd/decompress/zstd_ddict.c",
    "lib/zstd/decompress/zstd_decompress.c",
    "lib/zstd/decompress/zstd_decompress_block.c",
    "lib/zstd/zstd_common_module.c",
    "lib/zstd/zstd_compress_module.c",
    "lib/zstd/zstd_decompress_module.c",
    "kernel/capability.c",
    "kernel/cred.c",
    "kernel/groups.c",
    "kernel/locking/mutex.c",
    "kernel/locking/osq_lock.c",
    "kernel/locking/percpu-rwsem.c",
    "kernel/locking/rwsem.c",
    "kernel/locking/semaphore.c",
    "kernel/locking/spinlock.c",
    "kernel/notifier.c",
    "kernel/nscommon.c",
    "kernel/nsproxy.c",
    "kernel/nstree.c",
    "kernel/pid.c",
    "kernel/rcu/sync.c",
    "kernel/time/timeconv.c",
    "kernel/ucount.c",
    "kernel/user.c",
    "init/init_task.c",
    "mm/backing-dev.c",
    "mm/filemap.c",
    "mm/folio-compat.c",
    "mm/init-mm.c",
    "mm/list_lru.c",
    "mm/mempool.c",
    "mm/mmzone.c",
    "mm/page-writeback.c",
    "mm/readahead.c",
    "mm/swap.c",
    "mm/truncate.c",
    "mm/util.c",
    "mm/vmstat.c",
    "mm/workingset.c",
    "block/bdev.c",
    "block/bio.c",
    "block/fops.c",
    "drivers/base/map.c",
    "fs/attr.c",
    "fs/bad_inode.c",
    "fs/char_dev.c",
    "fs/d_path.c",
    "fs/dcache.c",
    "fs/fcntl.c",
    "fs/file.c",
    "fs/file_attr.c",
    "fs/file_table.c",
    "fs/filesystems.c",
    "fs/fs-writeback.c",
    "fs/fs_context.c",
    "fs/fs_parser.c",
    "fs/fs_pin.c",
    "fs/fs_struct.c",
    "fs/fs_types.c",
    "fs/init.c",
    "fs/inode.c",
    "fs/ioctl.c",
    "fs/iomap/buffered-io.c",
    "fs/iomap/direct-io.c",
    "fs/iomap/fiemap.c",
    "fs/iomap/ioend.c",
    "fs/iomap/iter.c",
    "fs/iomap/seek.c",
    "fs/libfs.c",
    "fs/locks.c",
    "fs/mnt_idmapping.c",
    "fs/namei.c",
    "fs/namespace.c",
    "fs/nsfs.c",
    "fs/open.c",
    "fs/pipe.c",
    "fs/pnode.c",
    "fs/ramfs/file-nommu.c",
    "fs/ramfs/inode.c",
    "fs/read_write.c",
    "fs/readdir.c",
    "fs/remap_range.c",
    "fs/seq_file.c",
    "fs/splice.c",
    "fs/stack.c",
    "fs/stat.c",
    "fs/statfs.c",
    "fs/super.c",
    "fs/sync.c",
    "fs/utimes.c",
    "fs/xattr.c",
    "security/commoncap.c",
    "crypto/algapi.c",
    "crypto/api.c",
    "crypto/blake2b_generic.c",
    "crypto/crc32c.c",
    "crypto/sha256.c",
    "crypto/shash.c",
    "crypto/xor.c",
    "crypto/xxhash_generic.c",
];

/// The sections that the shim walks, renamed from the release's to names
/// that the linker gives start and stop symbols: the initcalls of each
/// level, and the static per-CPU variables.
const RENAMES: &[(&str, &str)] = &[
    (".initcallearly.init", "mikros_initcall_early"),
    (".initcall0.init", "mikros_initcall0"),
    (".initcall0s.init", "mikros_initcall0"),
    (".initcall1.init", "mikros_initcall1"),
    (".initcall1s.init", "mikros_initcall1"),
    (".initcall2.init", "mikros_initcall2"),
    (".initcall2s.init", "mikros_initcall2"),
    (".initcall3.init", "mikros_initcall3"),
    (".initcall3s.init", "mikros_initcall3"),
    (".initcall4.init", "mikros_initcall4"),
    (".initcall4s.init", "mikros_initcall4"),
    (".initcall5.init", "mikros_initcall5"),
    (".initcall5s.init", "mikros_initcall5"),
    (".initcall6.init", "mikros_initcall6"),
    (".initcall6s.init", "mikros_initcall6"),
    (".initcall7.init", "mikros_initcall7"),
    (".initcall7s.init", "mikros_initcall7"),
    (".initcallrootfs.init", "mikros_initcall5"),
    (".data..percpu", "mikros_percpu"),
    (".data..percpu..first", "mikros_percpu"),
    (".data..percpu..page_aligned", "mikros_percpu"),
    (".data..percpu..read_mostly", "mikros_percpu"),
    (".data..percpu..shared_aligned", "mikros_percpu"),
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
    let shim = crate::workspace_root().join("user/btrfs/shim");
    let library = out.join("libbtrfs.a");
    let mut files = btrfs_files(&source)?;
    files.extend(LINUX.iter().map(|f| f.to_string()));
    files.extend(shim_files(&shim)?);
    let flags = flags(flavor, &source, &linux, &out);
    let stamp = digest(&flags, &files, &[&linux, &shim])?;
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
    crc32_table(&source, &linux, &out)?;
    let objects = compile(&source, &flags, &files, &out.join("obj"))?;
    let mut objcopy = Command::new(crate::llvm::tool("llvm-objcopy"));
    for (from, to) in RENAMES {
        objcopy.arg(format!("--rename-section={from}={to}"));
    }
    for object in &objects {
        crate::port::run(
            Command::new(objcopy.get_program())
                .args(objcopy.get_args())
                .arg(object),
        )?;
    }
    // One object, whose only global symbols are the shim's, so that the
    // release's names, such as strlen and snprintf, stay its own.
    let object = out.join("btrfs.o");
    let mut ld = Command::new(crate::llvm::tool("ld.lld"));
    ld.arg("-r").arg("-o").arg(&object).args(&objects);
    crate::port::run(&mut ld)?;
    let mut localize = Command::new(crate::llvm::tool("llvm-objcopy"));
    localize
        .args(["--wildcard", "--keep-global-symbol=mikros_*"])
        .arg(&object);
    crate::port::run(&mut localize)?;
    let mut ar = Command::new(crate::llvm::tool("llvm-ar"));
    ar.arg("rcs").arg(&library).arg(&object);
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

/// The shim's C files in `dir`, by their absolute paths.
fn shim_files(dir: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "c") {
            files.push(path.display().to_string());
        }
    }
    files.sort();
    Ok(files)
}

/// Writes `crc32table.h`, which the release generates with a program of
/// its own for the build host. The program includes the configuration
/// by a path from its own directory, so it is built in a directory laid
/// out as the tree is.
fn crc32_table(source: &Path, linux: &Path, out: &Path) -> Result<()> {
    let tree = out.join("crc32");
    for dir in ["lib/crc", "include/linux", "include/generated"] {
        fs::create_dir_all(tree.join(dir))?;
    }
    for (from, to) in [
        (
            source.join("lib/crc/gen_crc32table.c"),
            "lib/crc/gen_crc32table.c",
        ),
        (
            source.join("include/linux/crc32poly.h"),
            "include/linux/crc32poly.h",
        ),
        (
            linux.join("include/generated/autoconf.h"),
            "include/generated/autoconf.h",
        ),
    ] {
        fs::copy(&from, tree.join(to)).with_context(|| format!("copying {}", from.display()))?;
    }
    let generator = tree.join("gen_crc32table");
    crate::port::run(
        Command::new("cc")
            .arg("-O2")
            .arg("-o")
            .arg(&generator)
            .arg(tree.join("lib/crc/gen_crc32table.c")),
    )?;
    let table = crate::port::output(&mut Command::new(&generator))?;
    fs::write(out.join("include/crc32table.h"), table)?;
    Ok(())
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
        // Last, for the shim's files that include the tree's.
        source.to_owned(),
    ] {
        flags.push(format!("-I{}", path(&dir)));
    }
    // The compiler's own headers, such as the intrinsics that zstd uses,
    // come after the tree's.
    let resource = crate::llvm::bin().join("../lib/clang");
    let builtin = fs::read_dir(&resource)
        .ok()
        .and_then(|mut d| d.next())
        .and_then(|e| e.ok())
        .map(|e| e.path().join("include"));
    if let Some(builtin) = builtin {
        flags.push("-isystem".into());
        flags.push(path(&builtin));
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
            // Kbuild's, for clang, from scripts/Makefile.extrawarn.
            "-Wno-gnu",
            "-Wno-format-overflow-non-kprintf",
            "-Wno-format-truncation-non-kprintf",
            "-Wno-default-const-init-unsafe",
            "-Wno-unused-const-variable",
            "-Wno-format-overflow",
            "-Wno-pointer-to-enum-cast",
            "-Wno-tautological-constant-out-of-range-compare",
            "-Wno-unaligned-access",
            "-Wno-enum-compare-conditional",
            "-Wno-enum-enum-conversion",
        ]
        .map(String::from),
    );
    flags
}

/// A digest of everything a build depends on that the source's own
/// stamp does not cover: how it is built, the flags, the files, and the
/// files in `dirs`, the Mikros architecture's and the shim's.
fn digest(flags: &[String], files: &[String], dirs: &[&Path]) -> Result<String> {
    let mut hasher = Sha256::new();
    // The steps of the build are this file's.
    hasher.update(include_str!("btrfs.rs"));
    for item in flags.iter().chain(files) {
        hasher.update(item.as_bytes());
        hasher.update([0]);
    }
    let mut paths = Vec::new();
    for dir in dirs {
        collect(dir, &mut paths)?;
    }
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
        .map(|f| {
            // The shim's files come by absolute paths, the tree's relative.
            let name = match Path::new(f).strip_prefix("/") {
                Ok(_) => format!(
                    "shim_{}",
                    Path::new(f).file_name().unwrap().to_string_lossy()
                ),
                Err(_) => f.replace('/', "_"),
            };
            dir.join(name).with_extension("o")
        })
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
