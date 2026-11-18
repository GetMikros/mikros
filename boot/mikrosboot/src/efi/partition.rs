//! Finding the system partition on the boot disk, and reading the kernel
//! and the boot list of the system version to boot from it.

use super::Failure;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use mikrosboot::attempts::{self, Attempts};
use mikrosboot::bootlist::{self, Module};
use mikrosboot::btrfs::{Node, Volume};
use mikrosboot::disk::{Disk, IoError, Slice};
use mikrosboot::{cmdline, gpt, version};
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::ProtocolPointer;
use uefi::proto::device_path::DevicePath;
use uefi::proto::media::block::BlockIO;
use uefi::proto::media::disk::DiskIo;
use uefi::runtime::{self, VariableAttributes, VariableVendor};
use uefi::{CString16, Guid, Handle, Status};

/// The device path type and subtype of a hard drive media node.
const MEDIA: u8 = 4;
const HARD_DRIVE: u8 = 1;

/// The boot disk, read through `EFI_DISK_IO_PROTOCOL`.
pub struct BootDisk {
    io: ScopedProtocol<DiskIo>,
    media: u32,
}

impl Disk for BootDisk {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        self.io
            .read_disk(self.media, offset, buf)
            .map_err(|e| IoError(e.status().0))
    }
}

/// The file system of the system partition.
pub type System = Volume<Slice<BootDisk>>;

/// What the system partition gives the kernel.
pub struct Found {
    pub volume: System,
    pub kernel: Vec<u8>,
    pub modules: Vec<Module>,
    /// The command line with the system partition and the version.
    pub cmdline: String,
}

/// The protocol `P` of `handle`, without taking it from its driver.
fn protocol<P: ProtocolPointer + ?Sized>(handle: Handle) -> Option<ScopedProtocol<P>> {
    let params = OpenProtocolParams {
        handle,
        agent: boot::image_handle(),
        controller: None,
    };
    // SAFETY: GetProtocol takes the protocol from no driver, and each
    // scope ends before boot services end.
    unsafe { boot::open_protocol::<P>(params, OpenProtocolAttributes::GetProtocol) }.ok()
}

/// The nodes of `path` as type, subtype and data.
fn nodes(path: &DevicePath) -> Vec<(u8, u8, Vec<u8>)> {
    path.node_iter()
        .map(|n| (n.device_type().0, n.sub_type().0, n.data().to_vec()))
        .collect()
}

/// The location of the disk `handle` with the device path `nodes` for
/// `boot_disk=` (MIKSPEC-0004.R40), or None, with a warning, when its path
/// leads to no PCI function or holds a node that no part of a location
/// stands for.
fn location(handle: Handle, nodes: &[(u8, u8, Vec<u8>)]) -> Option<String> {
    let Some(pci) = super::pci::on_path(handle) else {
        super::warn(format_args!(
            "the boot disk is on no PCI function; boot_disk= is left out"
        ));
        return None;
    };
    match mikrosboot::location::location(pci, nodes) {
        Ok(location) => Some(location),
        Err(e) => {
            super::warn(format_args!(
                "the boot disk's device path has a node of type {} and subtype {:#x} \
                 after its PCI nodes; boot_disk= is left out",
                e.kind, e.sub
            ));
            None
        }
    }
}

/// The boot disk, whose device path is that of `device`, the bootloader's
/// device, without its last node, which must be a hard drive media node;
/// with its block size, last block and location.
fn boot_disk(device: Handle) -> Result<(BootDisk, u32, u64, Option<String>), Failure> {
    let step = "finding the boot disk";
    let path = protocol::<DevicePath>(device)
        .ok_or_else(|| Failure::new(step, "the boot device has no device path"))?;
    let mut wanted = nodes(&path);
    if !matches!(wanted.pop(), Some((MEDIA, HARD_DRIVE, _))) {
        return Err(Failure::new(step, "the bootloader is not on a partition"));
    }
    let handles = boot::find_handles::<BlockIO>().map_err(|e| Failure::new(step, e))?;
    for handle in handles {
        let same = protocol::<DevicePath>(handle).is_some_and(|p| nodes(&p) == wanted);
        if !same {
            continue;
        }
        let (Some(block), Some(io)) = (protocol::<BlockIO>(handle), protocol::<DiskIo>(handle))
        else {
            return Err(Failure::new(step, "the boot disk has no disk I/O"));
        };
        let media = block.media();
        let disk = BootDisk {
            io,
            media: media.media_id(),
        };
        let at = location(handle, &wanted);
        return Ok((disk, media.block_size(), media.last_block(), at));
    }
    Err(Failure::new(step, "no disk has the boot partition's path"))
}

/// The command-line word that leaves out the drivers that packages
/// installed for boot.
const NODRIVERS: &str = "nodrivers";

/// The version to boot of `current` and `previous`, counted in the
/// `MikrosBoot` variable.
fn count(current: &str, previous: Option<&str>) -> String {
    let name = CString16::try_from(attempts::NAME).expect("an ASCII name");
    let vendor = VariableVendor(Guid::from_bytes(attempts::VENDOR));
    let read = match runtime::get_variable_boxed(&name, &vendor) {
        Ok((data, attributes)) => Ok(Attempts::decode(attributes.bits(), &data)),
        Err(e) if e.status() == Status::NOT_FOUND => Ok(None),
        Err(_) => Err(()),
    };
    let choice = attempts::choose(current, previous, read);
    if let Some(write) = choice.write {
        let attributes = VariableAttributes::from_bits_retain(attempts::ATTRIBUTES);
        // When the write fails, the version boots without being counted.
        let _ = runtime::set_variable(&name, &vendor, attributes, &write.encode());
    }
    choice.version
}

/// The contents of the file at `path` in the version whose root is `root`.
fn read(volume: &mut System, root: Node, path: &'static str) -> Result<Vec<u8>, Failure> {
    let file = bootlist::file(volume, root, path).map_err(|e| Failure::new(path, e))?;
    let mut contents = vec![0; file.size as usize];
    volume
        .read(file.node, &mut contents)
        .map_err(|e| Failure::new(path, e))?;
    Ok(contents)
}

/// Finds the system partition on the boot disk of `device`, as `cmdline`
/// names it, chooses the version to boot, and reads its kernel, the
/// files its boot list names, and the drivers marked for boot. The
/// command line gets the partition and the boot disk's location.
pub fn find(device: Handle, cmdline: String) -> Result<Found, Failure> {
    let (mut disk, block_size, last_lba, location) = boot_disk(device)?;
    let step = "finding the system partition";
    let partitions =
        gpt::partitions(&mut disk, block_size, last_lba).map_err(|e| Failure::new(step, e))?;
    let wanted = match cmdline::root_partition(&cmdline) {
        Some(Ok(guid)) => Some(guid),
        Some(Err(_)) => {
            let candidates = gpt::roots(&partitions);
            return Err(Failure::new(step, gpt::Error::BadGuid { candidates }));
        }
        None => None,
    };
    let partition =
        gpt::system_partition(&partitions, wanted).map_err(|e| Failure::new(step, e))?;
    let (start, size) = partition
        .range(block_size, last_lba)
        .ok_or_else(|| Failure::new(step, "the partition lies outside the disk"))?;
    let cmdline = cmdline::with_root(cmdline, &partition.guid);
    let cmdline = match &location {
        Some(at) => cmdline::with_boot_disk(cmdline, at),
        None => cmdline,
    };
    let mut volume = Volume::open(Slice { disk, start, size })
        .map_err(|e| Failure::new("reading the system partition", e))?;
    let mut fell_back = false;
    let counted = |current: &str, previous: Option<&str>| {
        let name = count(current, previous);
        fell_back = name != current;
        name
    };
    let version = version::choose(&mut volume, &cmdline, counted)
        .map_err(|e| Failure::new("choosing the system version", e))?;
    let cmdline = cmdline::with_system(&cmdline, &version.name);
    cmdline::check_len(&cmdline).map_err(|e| Failure::new("finding the command line", e))?;
    let kernel = read(&mut volume, version.root, "mikros")?;
    let list = read(&mut volume, version.root, "boot/modules")?;
    let mut modules = bootlist::modules(&mut volume, version.root, &list)
        .map_err(|e| Failure::new("following boot/modules", e))?;
    // The drivers that packages installed for boot, except with
    // `nodrivers` and when the previous version boots after the current
    // one failed.
    let nodrivers = cmdline.split_whitespace().any(|w| w == NODRIVERS);
    if !nodrivers && !fell_back {
        let drivers = bootlist::drivers(&mut volume)
            .map_err(|e| Failure::new("following drivers/boot-modules", e))?;
        modules.extend(drivers);
    }
    Ok(Found {
        volume,
        kernel,
        modules,
        cmdline,
    })
}
