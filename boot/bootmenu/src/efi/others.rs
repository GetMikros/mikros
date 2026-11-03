//! Finding other systems in the firmware's load options and on the boot
//! volume.

use super::launch;
use alloc::boxed::Box;
use alloc::format;
use alloc::vec::Vec;
use bootmenu::detect::{self, Disks};
use bootmenu::entries::Config;
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::fs::FileSystem;
use uefi::proto::ProtocolPointer;
use uefi::proto::device_path::build::DevicePathBuilder;
use uefi::proto::device_path::{DevicePath, DeviceSubType, DeviceType};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::runtime::{self, VariableVendor};
use uefi::{CString16, Handle, cstr16};

/// A system found outside the entries file.
pub type Detected = detect::Detected<Box<DevicePath>>;

/// Opens protocol `P` on `handle` without taking it over from its users.
fn open_shared<P: ProtocolPointer + ?Sized>(handle: Handle) -> Option<ScopedProtocol<P>> {
    let params = OpenProtocolParams {
        handle,
        agent: boot::image_handle(),
        controller: None,
    };
    // SAFETY: GetProtocol does not take ownership of the protocol, and the
    // returned value is dropped before an entry is started.
    unsafe { boot::open_protocol::<P>(params, OpenProtocolAttributes::GetProtocol) }.ok()
}

fn is_file_node(node: &uefi::proto::device_path::DevicePathNode) -> bool {
    node.device_type() == DeviceType::MEDIA && node.sub_type() == DeviceSubType::MEDIA_FILE_PATH
}

/// The file system handle that `path` leads to, when the rest of `path`
/// after it is a file path.
fn volume_of(path: &DevicePath) -> Option<Handle> {
    let mut rest = path;
    let handle = boot::locate_device_path::<SimpleFileSystem>(&mut rest).ok()?;
    rest.node_iter().all(is_file_node).then_some(handle)
}

/// Expands a device path that starts with a hard drive node, as firmware
/// load options often do, by putting the device path of the partition with
/// the same node in front of the rest.
fn expand(path: &DevicePath) -> Option<Box<DevicePath>> {
    let first = path.node_iter().next()?;
    if first.device_type() != DeviceType::MEDIA
        || first.sub_type() != DeviceSubType::MEDIA_HARD_DRIVE
    {
        return None;
    }
    for handle in boot::find_handles::<SimpleFileSystem>().ok()? {
        let Some(volume) = open_shared::<DevicePath>(handle) else {
            continue;
        };
        let Some(position) = volume.node_iter().position(|node| node == first) else {
            continue;
        };
        let mut buf = Vec::new();
        let mut builder = DevicePathBuilder::with_vec(&mut buf);
        for node in volume
            .node_iter()
            .take(position + 1)
            .chain(path.node_iter().skip(1))
        {
            builder = builder.push(&node).ok()?;
        }
        return builder.finalize().ok().map(DevicePath::to_boxed);
    }
    None
}

/// Reads the file at `path` on the volume `handle`.
fn read(handle: Handle, path: &str) -> Option<Vec<u8>> {
    let volume = open_shared::<SimpleFileSystem>(handle)?;
    let name = CString16::try_from(path).ok()?;
    FileSystem::new(volume).read(name.as_ref()).ok()
}

/// Whether the file at `path` on the volume `handle` exists.
fn exists(handle: Handle, path: &str) -> bool {
    let (Some(volume), Ok(name)) = (
        open_shared::<SimpleFileSystem>(handle),
        CString16::try_from(path),
    ) else {
        return false;
    };
    FileSystem::new(volume).try_exists(name.as_ref()) == Ok(true)
}

/// The load options and volumes of the firmware, with the device path of
/// the boot volume.
struct Firmware<'a> {
    boot_path: &'a DevicePath,
}

impl Disks for Firmware<'_> {
    type Volume = Handle;
    type DevicePath = Box<DevicePath>;

    fn boot_order(&mut self) -> Vec<u16> {
        let order =
            runtime::get_variable_boxed(cstr16!("BootOrder"), &VariableVendor::GLOBAL_VARIABLE);
        order
            .map(|(data, _)| {
                data.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&c| u16::from_le_bytes(c))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn load_option(&mut self, number: u16) -> Option<detect::LoadOption> {
        let name = CString16::try_from(format!("Boot{number:04X}").as_str()).ok()?;
        let (data, _) =
            runtime::get_variable_boxed(&name, &VariableVendor::GLOBAL_VARIABLE).ok()?;
        detect::parse_load_option(&data)
    }

    fn locate(&mut self, device_path: &[u8]) -> Option<(Box<DevicePath>, Option<Handle>)> {
        let device_path = <&DevicePath>::try_from(device_path).ok()?;
        let full = expand(device_path).unwrap_or_else(|| device_path.to_boxed());
        let volume = volume_of(&full);
        Some((full, volume))
    }

    fn read(&mut self, volume: Handle, path: &str) -> Option<Vec<u8>> {
        read(volume, path)
    }

    fn exists(&mut self, volume: Handle, path: &str) -> bool {
        exists(volume, path)
    }

    fn file_device_path(&mut self, path: &str) -> Option<Box<DevicePath>> {
        launch::file_device_path(self.boot_path, path).ok()
    }
}

/// Finds the systems to add after the entries of `config`. `boot_path` is
/// the device path of the boot volume.
pub fn find(config: &Config, boot_path: &DevicePath) -> Vec<Detected> {
    let Some(boot_volume) = volume_of(boot_path) else {
        return Vec::new();
    };
    detect::find(&mut Firmware { boot_path }, config, boot_volume)
}
