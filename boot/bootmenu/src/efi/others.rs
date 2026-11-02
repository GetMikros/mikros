//! Finding other systems in the firmware's load options and on the boot
//! volume.

use super::launch;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use bootmenu::detect::{self, FALLBACK_PATH, KNOWN_IMAGES, MIKROS_SECTION, Started};
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
pub struct Detected {
    /// Not shown, but kept unique among all entries like the IDs of the
    /// entries file.
    #[allow(dead_code)]
    pub id: String,
    pub name: String,
    /// The path of the image on its volume, used to name it in notices.
    pub path: String,
    /// Whether the image is on a volume other than the boot volume.
    pub other_disk: bool,
    /// The full device path of the image.
    pub device_path: Box<DevicePath>,
}

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

/// Whether the file at `path` on `volume` is a Mikros boot file.
fn is_mikros_boot_file(volume: Option<Handle>, path: &str) -> bool {
    detect::in_mikros_dir(path)
        || (detect::same_file(path, FALLBACK_PATH)
            && volume
                .and_then(|v| read(v, path))
                .is_some_and(|image| detect::has_pe_section(&image, MIKROS_SECTION)))
}

/// The load option numbers in `BootOrder`.
fn boot_order() -> Vec<u16> {
    let order = runtime::get_variable_boxed(cstr16!("BootOrder"), &VariableVendor::GLOBAL_VARIABLE);
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

/// Reads the load option `Boot####` with the given number.
fn load_option(number: u16) -> Option<detect::LoadOption> {
    let name = CString16::try_from(format!("Boot{number:04X}").as_str()).ok()?;
    let (data, _) = runtime::get_variable_boxed(&name, &VariableVendor::GLOBAL_VARIABLE).ok()?;
    detect::parse_load_option(&data)
}

/// Finds the systems to add after the entries of `config`. `boot_path` is
/// the device path of the boot volume.
pub fn find(config: &Config, boot_path: &DevicePath) -> Vec<Detected> {
    let Some(boot_volume) = volume_of(boot_path) else {
        return Vec::new();
    };
    let mut started = Started::new();
    for entry in &config.entries {
        started.insert(boot_volume, &entry.path);
    }

    let mut found: Vec<(String, String, Option<Handle>, Box<DevicePath>)> = Vec::new();
    for number in boot_order() {
        let Some(option) = load_option(number) else {
            continue;
        };
        let Some(path) = detect::file_path(&option.device_path) else {
            continue;
        };
        let Ok(device_path) = <&DevicePath>::try_from(option.device_path.as_slice()) else {
            continue;
        };
        let full = expand(device_path).unwrap_or_else(|| device_path.to_boxed());
        let volume = volume_of(&full);
        if is_mikros_boot_file(volume, &path) {
            continue;
        }
        // An option whose volume is not present cannot be compared with
        // other entries, so it is always added.
        if let Some(volume) = volume
            && !started.insert(volume, &path)
        {
            continue;
        }
        found.push((option.description, path, volume, full));
    }

    for (path, name) in KNOWN_IMAGES {
        if started.contains(&boot_volume, path) || !exists(boot_volume, path) {
            continue;
        }
        let Ok(full) = launch::file_device_path(boot_path, path) else {
            continue;
        };
        started.insert(boot_volume, path);
        found.push((
            String::from(name),
            String::from(path),
            Some(boot_volume),
            full,
        ));
    }

    let mut ids = detect::ids(|id| config.entries.iter().any(|e| e.id == id));
    found
        .into_iter()
        .map(|(name, path, volume, device_path)| Detected {
            id: ids.next().unwrap_or_default(),
            name,
            path,
            other_disk: volume != Some(boot_volume),
            device_path,
        })
        .collect()
}
