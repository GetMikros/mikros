//! `cargo xtask image`: creates a bootable GPT disk image.

mod demos;
pub(crate) mod esp;
pub(crate) mod gpt;
pub(crate) mod owners;
pub mod root;

use crate::build::{self, Edition, Kind};
use crate::secure_boot::{self, Key};
use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) const ESP_TYPE: Uuid = Uuid::from_u128(0xc12a7328_f81f_11d2_ba4b_00a0c93ec93b);
const ROOT_TYPE: Uuid = Uuid::from_u128(0xfa730af6_8ba3_42d9_b155_466909d08135);
/// The types of the small partitions that the test machine's disk has
/// after the root partition, for the test drivers test-fat and
/// test-btrfs, which bind to them as nothing else matches them.
const TEST_TYPES: [(Uuid, &str); 2] = [
    (
        Uuid::from_u128(0xeb1f4cf2_5884_4f87_880c_3dd4d8286764),
        "test-fat",
    ),
    (
        Uuid::from_u128(0x38a2aae9_c051_4fe5_a24a_441d4a220d2a),
        "test-btrfs",
    ),
];

#[derive(clap::Args)]
pub struct Args {
    /// The build kind to put in the image.
    #[arg(long, value_enum, default_value_t = Kind::Debug)]
    pub kind: Kind,
    /// The edition of the system: `desktop`, or `server`, whose users log
    /// in on the text console and which has no compositor, programs that
    /// start the desktop, or libraries that only they need. For now the
    /// desktop edition has stand-ins for the greeter and the shell and the
    /// administrator `dee`, whom the greeter logs in on text console 1,
    /// where `desktop` starts the desktop; Ctrl-Alt-F1 shows the desktop and
    /// Ctrl-Alt-F2 the console. The password of `dee` is made for each
    /// image, printed, and written to a file next to the image. The
    /// production kind has no stand-ins, and its desktop edition keeps the
    /// desktop off.
    /// Boot an image with `cargo xtask run`, with the same edition.
    #[arg(long, value_enum, default_value_t = Edition::Desktop, conflicts_with = "tests")]
    pub edition: Edition,
    /// Install the files of DIRECTORY, laid out as /system, in the image:
    /// `bin/` in /system/bin, `libraries/*.dlib` in /system/libraries and
    /// `share/` in /system/share, so that programs and libraries built
    /// outside this repository can run, for example on the desktop edition:
    /// start them in the text console after `desktop`, and press Ctrl-Alt-F1
    /// to see a window. Debug information is stripped; each program and
    /// library must record its ABI levels, a library's soname must be its
    /// file name, and no file replaces one of the system.
    #[arg(long, value_name = "DIRECTORY", conflicts_with = "tests")]
    pub demos: Option<PathBuf>,
    /// Create a live image, whose command line holds the word `live`, so
    /// that the system uses its root partition read-only and keeps no
    /// changes.
    #[arg(long)]
    pub live: bool,
    /// Sign the EFI executables for Secure Boot and add the files for
    /// enrolling the signing key in the firmware.
    #[arg(long)]
    pub signed: bool,
    /// The directory of the signing key, instead of
    /// ~/.local/share/mikros/secure-boot.
    #[arg(long, value_name = "DIRECTORY", requires = "signed")]
    pub key: Option<PathBuf>,
    /// Create a test image: the system with its system tests and
    /// selftest, which runs them.
    #[arg(long)]
    pub tests: bool,
}

/// Where the image of a build kind is written: one of the desktop edition
/// with `edition`, a live one with `live`, a test image with `tests`, which
/// has no edition.
pub fn path(kind: Kind, edition: Edition, live: bool, tests: bool) -> PathBuf {
    let name = format!(
        "mikros-{}{}{}{}.img",
        kind.name(),
        if edition == Edition::Desktop && !tests {
            "-desktop"
        } else {
            ""
        },
        if live { "-live" } else { "" },
        if tests { "-tests" } else { "" }
    );
    crate::workspace_root().join("target/image").join(name)
}

/// The contents of a partition.
enum Data<'a> {
    Bytes(&'a [u8]),
    File(&'a Path),
}

impl Data<'_> {
    fn len(&self) -> Result<u64> {
        Ok(match self {
            Data::Bytes(bytes) => bytes.len() as u64,
            Data::File(path) => fs::metadata(path)?.len(),
        })
    }
}

struct Part<'a> {
    type_guid: Uuid,
    guid: Uuid,
    name: &'static str,
    data: Data<'a>,
}

/// Copies `from` to the current position of `to`, seeking over chunks of
/// zeros instead of writing them, so that the image stays sparse. `to` must
/// already be zero there.
fn copy_sparse(from: &mut File, to: &mut File) -> io::Result<()> {
    let mut chunk = vec![0u8; 1 << 16];
    loop {
        let n = from.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        if chunk[..n].iter().all(|&b| b == 0) {
            to.seek(SeekFrom::Current(n as i64))?;
        } else {
            to.write_all(&chunk[..n])?;
        }
    }
}

/// Writes a disk image at `path` holding `parts` in order.
fn write_image(path: &Path, parts: &[Part]) -> Result<()> {
    let sizes = parts
        .iter()
        .map(|p| p.data.len())
        .collect::<Result<Vec<_>>>()?;
    let (places, sectors) = gpt::layout(&sizes);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut disk = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    disk.set_len(sectors * gpt::SECTOR)?;
    let table: Vec<gpt::Partition> = parts
        .iter()
        .zip(&places)
        .map(|(p, &(first_lba, last_lba))| gpt::Partition {
            type_guid: p.type_guid,
            guid: p.guid,
            name: p.name,
            first_lba,
            last_lba,
        })
        .collect();
    gpt::write(&mut disk, sectors, Uuid::new_v4(), &table)?;
    for (part, &(first_lba, _)) in parts.iter().zip(&places) {
        disk.seek(SeekFrom::Start(first_lba * gpt::SECTOR))?;
        match &part.data {
            Data::Bytes(bytes) => disk.write_all(bytes)?,
            Data::File(file) => copy_sparse(&mut File::open(file)?, &mut disk)?,
        }
    }
    disk.sync_all()?;
    Ok(())
}

pub fn run(args: &Args) -> Result<()> {
    let key = match (args.signed, &args.key) {
        (false, _) => None,
        (true, Some(dir)) => Some(secure_boot::key::load_or_create(dir)?),
        (true, None) => Some(secure_boot::key::load_or_create(
            &secure_boot::key::default_dir()?,
        )?),
    };
    // A directory that cannot be installed fails before the build.
    let demos = match &args.demos {
        Some(dir) => demos::collect(dir)?,
        None => Vec::new(),
    };
    let built = match args.tests {
        true => build::build_tests(args.kind)?,
        false => build::build_edition(args.kind, args.edition)?,
    };
    let mut files = Vec::new();
    for demo in demos {
        files.push((demo.at, build::without_debug_info(args.kind, &demo.source)?));
    }
    if !files.is_empty() {
        let bytes: u64 = files
            .iter()
            .map(|(_, p)| fs::metadata(p).map_or(0, |m| m.len()))
            .sum();
        println!(
            "demos    {} files, {} MiB",
            files.len(),
            bytes.div_ceil(1 << 20)
        );
    }
    let out = path(args.kind, args.edition, args.live, args.tests);
    // An image for installing or releasing renews its partition GUIDs at
    // its first boot, so that the disks copied from it do not share them;
    // a test image keeps them (MIKSPEC-0023.R48).
    let mut words = Vec::new();
    if args.live {
        words.push(LIVE);
    }
    if !args.tests {
        words.push(RENEW_GUIDS);
    }
    create_parts(
        args.kind,
        &built,
        &built.system_image,
        &out,
        &words.join(" "),
        &[],
        Extras {
            demos: &files,
            ..Extras::default()
        },
        key.as_ref(),
    )?;
    if args.kind != Kind::Production && (args.tests || args.edition == Edition::Desktop) {
        match (&built.stand_in_account, args.tests) {
            (Some(account), false) => {
                let copy = out.with_extension("password");
                fs::copy(&account.file, &copy)
                    .with_context(|| format!("writing {}", copy.display()))?;
                println!(
                    "stand-ins: this image holds stand-ins for the greeter and the shell, and the \
                     account {}, whose password is {}, which is also in {}, a secret of \
                     development (MIKSPEC-0023.R59)",
                    account.names.join(", "),
                    account.password,
                    copy.display()
                );
            }
            _ => println!(
                "stand-ins: this image holds stand-ins for the greeter and the shell, for \
                 development only (MIKSPEC-0023.R59)"
            ),
        }
    }
    if args.kind == Kind::Staging {
        println!(
            "staging image: built as production is, with the stand-ins of its edition; for \
             development and testing, not production, and not to be published \
             (MIKSPEC-0023.R60)"
        );
    }
    Ok(())
}

/// The command line word that makes the system use the root partition
/// read-only, as a live system.
pub const LIVE: &str = "live";

/// The word of `boot.conf` that makes the bootloader give the disk new
/// partition GUIDs at the next boot (MIKSPEC-0004.R41).
pub const RENEW_GUIDS: &str = "renew_guids";

/// The files of the EFI system partition: the boot manager, also at the
/// fallback path, the bootloader and `command_line`. With `key`, the EFI
/// executables are signed with it and the files for enrolling it are
/// added.
fn esp_files(
    built: &build::Built,
    command_line: String,
    key: Option<&Key>,
) -> Result<Vec<(String, Vec<u8>)>> {
    let read = |path: &Path| fs::read(path).with_context(|| format!("reading {}", path.display()));
    let bootmenu = read(&built.bootmenu)?;
    let mut files: Vec<(String, Vec<u8>)> = [
        ("EFI/Mikros/bootmenu.efi", bootmenu.clone()),
        ("EFI/BOOT/BOOTX64.EFI", bootmenu),
        ("EFI/Mikros/mikrosboot.efi", read(&built.bootloader)?),
        ("EFI/Mikros/boot.conf", command_line.into_bytes()),
    ]
    .into_iter()
    .map(|(path, data)| (path.to_owned(), data))
    .collect();
    if let Some(key) = key {
        secure_boot::sign_esp(&mut files, key)?;
    }
    Ok(files)
}

/// Writes the disk image of the components `built` for the build kind
/// `kind` to `out`.
pub fn create(kind: Kind, built: &build::Built, out: &Path) -> Result<()> {
    create_with(kind, built, out, "", &[])
}

/// The command line word that makes the bootloader place the kernel at
/// its linked addresses.
pub const KASLR_OFF: &str = "kaslr=off";

/// Like `create`, with `extra` added to the kernel command line and
/// `more` files, given as paths and contents, on the EFI system
/// partition. A fuzz build always boots with `KASLR_OFF`, so that the
/// addresses its coverage records are those of the kernel image as
/// linked.
pub fn create_with(
    kind: Kind,
    built: &build::Built,
    out: &Path,
    extra: &str,
    more: &[(&str, &[u8])],
) -> Result<()> {
    create_parts(
        kind,
        built,
        &built.system_image,
        out,
        extra,
        more,
        Extras::default(),
        None,
    )
}

/// Like `create`, with the EFI executables signed with `key` and the
/// files for enrolling it on the EFI system partition.
pub fn create_signed(kind: Kind, built: &build::Built, out: &Path, key: &Key) -> Result<()> {
    create_parts(
        kind,
        built,
        &built.system_image,
        out,
        "",
        &[],
        Extras::default(),
        Some(key),
    )
}

/// Like `create_with`, for `cargo xtask test`: the disk also has the
/// partitions of `TEST_TYPES`.
pub fn create_for_tests(kind: Kind, built: &build::Built, out: &Path, extra: &str) -> Result<()> {
    create_parts(
        kind,
        built,
        &built.system_image,
        out,
        extra,
        &[],
        Extras {
            test_partitions: true,
            ..Extras::default()
        },
        None,
    )
}

/// Like `create_with`, with the root partition made from the archive
/// `system` instead of the built system.
pub fn create_with_system(
    kind: Kind,
    built: &build::Built,
    system: &Path,
    out: &Path,
    extra: &str,
) -> Result<()> {
    create_parts(
        kind,
        built,
        system,
        out,
        extra,
        &[],
        Extras::default(),
        None,
    )
}

/// Like `create_with`, with a copy of the system version as the one that
/// `@system/previous` names.
pub fn create_with_previous(
    kind: Kind,
    built: &build::Built,
    out: &Path,
    extra: &str,
) -> Result<()> {
    let extras = Extras {
        previous: true,
        ..Extras::default()
    };
    create_parts(
        kind,
        built,
        &built.system_image,
        out,
        extra,
        &[],
        extras,
        None,
    )
}

/// What a disk holds besides the system: the partitions of `TEST_TYPES`,
/// and a previous system version.
#[derive(Clone, Copy, Default)]
struct Extras<'a> {
    test_partitions: bool,
    previous: bool,
    /// The files that go in the system version, by their paths in it.
    demos: &'a [(String, PathBuf)],
}

#[allow(clippy::too_many_arguments)]
fn create_parts(
    kind: Kind,
    built: &build::Built,
    system: &Path,
    out: &Path,
    extra: &str,
    more: &[(&str, &[u8])],
    extras: Extras,
    key: Option<&Key>,
) -> Result<()> {
    // The root file system is built in `target/image` under a name of the
    // build kind, and then copied into `out`: one image at a time, for the
    // parallel lanes of cargo xtask test.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root_guid = Uuid::new_v4();
    let mut command_line = format!("root=PARTUUID={root_guid}");
    if !extra.is_empty() {
        command_line = format!("{command_line} {extra}");
    }
    if kind == Kind::Fuzz && !extra.split(' ').any(|w| w == KASLR_OFF) {
        command_line = format!("{command_line} {KASLR_OFF}");
    }
    let contents = esp_files(built, command_line, key)?;
    let files: Vec<(&str, &[u8])> = contents
        .iter()
        .map(|(p, d)| (p.as_str(), d.as_slice()))
        .chain(more.iter().copied())
        .collect();
    let total = files.iter().map(|(_, d)| d.len() as u64).sum();
    let esp = esp::build(esp::size_for(total), &files)?;
    let root = root::create(
        kind,
        system,
        &built.boot_image,
        &built.kernel,
        extras.previous,
        extras.demos,
    )?;
    let mut parts = vec![
        Part {
            type_guid: ESP_TYPE,
            guid: Uuid::new_v4(),
            name: "EFI system partition",
            data: Data::Bytes(&esp),
        },
        Part {
            type_guid: ROOT_TYPE,
            guid: root_guid,
            name: "Mikros",
            data: Data::File(&root),
        },
    ];
    let test = [0; 4096];
    if extras.test_partitions {
        for (type_guid, name) in TEST_TYPES {
            parts.push(Part {
                type_guid,
                guid: Uuid::new_v4(),
                name,
                data: Data::Bytes(&test),
            });
        }
    }
    write_image(out, &parts)?;
    println!("created  {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    #[test]
    fn names_the_image_of_each_edition() {
        let name = |edition, live, tests| {
            path(Kind::Debug, edition, live, tests)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(name(Edition::Server, false, false), "mikros-debug.img");
        assert_eq!(
            name(Edition::Desktop, false, false),
            "mikros-debug-desktop.img"
        );
        assert_eq!(
            name(Edition::Desktop, true, false),
            "mikros-debug-desktop-live.img"
        );
        assert_eq!(name(Edition::Server, false, true), "mikros-debug-tests.img");
        assert_eq!(
            name(Edition::Desktop, false, true),
            "mikros-debug-tests.img"
        );
    }

    #[test]
    fn a_test_image_has_no_edition() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["image"]).is_ok());
        assert!(Cli::try_parse_from(["image", "--tests"]).is_ok());
        assert!(Cli::try_parse_from(["image", "--edition", "desktop"]).is_ok());
        assert!(Cli::try_parse_from(["image", "--tests", "--edition", "desktop"]).is_err());
        assert!(Cli::try_parse_from(["image", "--edition", "kiosk"]).is_err());
    }

    #[test]
    fn writes_partitions_at_their_places() {
        let dir = std::env::temp_dir().join(format!("xtask-image-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let second = dir.join("second.bin");
        // A chunk of zeros, which is skipped, followed by data.
        let mut contents = vec![0u8; 1 << 16];
        contents.extend_from_slice(b"root");
        fs::write(&second, &contents).unwrap();
        let image = dir.join("disk.img");
        let parts = [
            Part {
                type_guid: ESP_TYPE,
                guid: Uuid::from_u128(1),
                name: "a",
                data: Data::Bytes(b"esp"),
            },
            Part {
                type_guid: ROOT_TYPE,
                guid: Uuid::from_u128(2),
                name: "b",
                data: Data::File(&second),
            },
        ];
        write_image(&image, &parts).unwrap();
        let bytes = fs::read(&image).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(bytes.len() as u64, (6144 + 33) * gpt::SECTOR);
        assert_eq!(&bytes[512..520], b"EFI PART");
        assert_eq!(&bytes[2048 * 512..][..3], b"esp");
        assert!(bytes[4096 * 512..][..1 << 16].iter().all(|&b| b == 0));
        assert_eq!(&bytes[4096 * 512 + (1 << 16)..][..4], b"root");
    }
}
