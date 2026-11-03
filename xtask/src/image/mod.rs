//! `cargo xtask image`: creates a bootable GPT disk image.

mod esp;
mod gpt;
mod root;

use crate::build::{self, Kind};
use anyhow::{Context, Result, bail};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const ESP_TYPE: Uuid = Uuid::from_u128(0xc12a7328_f81f_11d2_ba4b_00a0c93ec93b);
const ROOT_TYPE: Uuid = Uuid::from_u128(0xfa730af6_8ba3_42d9_b155_466909d08135);

#[derive(clap::Args)]
pub struct Args {
    /// The build kind to put in the image.
    #[arg(long, value_enum, default_value_t = Kind::Debug)]
    pub kind: Kind,
    /// Create an image with only the EFI system partition, which runs the
    /// whole system from the boot image.
    #[arg(long)]
    pub live: bool,
}

/// Where the image of a build kind is written.
pub fn path(kind: Kind, live: bool) -> PathBuf {
    let name = format!(
        "mikros-{}{}.img",
        kind.name(),
        if live { "-live" } else { "" }
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
    if args.live {
        bail!("live images are not supported yet: the boot image's contents are not defined");
    }
    let built = build::build(args.kind)?;
    create(args.kind, &built, &path(args.kind, false))
}

/// Writes the disk image of the components `built` for the build kind
/// `kind` to `out`.
pub fn create(kind: Kind, built: &build::Built, out: &Path) -> Result<()> {
    create_with(kind, built, out, "")
}

/// Like `create`, with `extra` added to the kernel command line.
pub fn create_with(kind: Kind, built: &build::Built, out: &Path, extra: &str) -> Result<()> {
    let root_guid = Uuid::new_v4();
    let read = |path: &Path| fs::read(path).with_context(|| format!("reading {}", path.display()));
    let bootmenu = read(&built.bootmenu)?;
    let mut command_line = format!("root=PARTUUID={root_guid}");
    if !extra.is_empty() {
        command_line = format!("{command_line} {extra}");
    }
    let contents = [
        ("EFI/Mikros/bootmenu.efi", bootmenu.clone()),
        ("EFI/BOOT/BOOTX64.EFI", bootmenu),
        ("EFI/Mikros/mikrosboot.efi", read(&built.bootloader)?),
        ("EFI/Mikros/kernel", read(&built.kernel)?),
        ("EFI/Mikros/boot.img", read(&built.boot_image)?),
        ("EFI/Mikros/boot.conf", command_line.into_bytes()),
    ];
    let files: Vec<(&str, &[u8])> = contents.iter().map(|(p, d)| (*p, d.as_slice())).collect();
    let total = files.iter().map(|(_, d)| d.len() as u64).sum();
    let esp = esp::build(esp::size_for(total), &files)?;
    let root = root::create(kind)?;

    write_image(
        out,
        &[
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
        ],
    )?;
    println!("created  {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
