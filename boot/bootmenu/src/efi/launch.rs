//! Starting the EFI image of an entry.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use bootmenu::launch;
use uefi::Status;
use uefi::boot::{self, LoadImageSource};
use uefi::proto::BootPolicy;
use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;

/// The device path of the volume the boot manager was loaded from.
pub fn boot_volume() -> Option<Box<DevicePath>> {
    let image = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle()).ok()?;
    let device = image.device()?;
    let path = boot::open_protocol_exclusive::<DevicePath>(device).ok()?;
    Some(path.to_boxed())
}

/// Describes why `path` could not be started.
fn reason(path: &str, status: Status) -> String {
    match status {
        Status::NOT_FOUND => format!("{path} not found"),
        status => format!("{path}: {status:?}"),
    }
}

/// The device path of the file at `path` on `volume`.
pub fn file_device_path(volume: &DevicePath, path: &str) -> Result<Box<DevicePath>, String> {
    let bytes = launch::file_device_path(volume.as_bytes(), path)?;
    <&DevicePath>::try_from(bytes.as_slice())
        .map(DevicePath::to_boxed)
        .map_err(|_| format!("{path}: invalid device path"))
}

/// Loads the image at `path` on `volume` and starts it with `args` as its
/// load options. Returns when the image exits, whatever its exit status, or
/// with the reason it could not be loaded or started.
pub fn start(volume: &DevicePath, path: &str, args: Option<&str>) -> Result<(), String> {
    start_image(&file_device_path(volume, path)?, path, args)
}

/// Loads the image at the full device path `full_path` and starts it with
/// `args` as its load options, as `start` does. `path` names the image in
/// reasons. A status from `StartImage` counts as a failure to start only
/// when `launch::start_refused` says so.
pub fn start_image(full_path: &DevicePath, path: &str, args: Option<&str>) -> Result<(), String> {
    let source = LoadImageSource::FromDevicePath {
        device_path: full_path,
        boot_policy: BootPolicy::ExactMatch,
    };
    let image =
        boot::load_image(boot::image_handle(), source).map_err(|e| reason(path, e.status()))?;

    // The options must stay alive until the image has exited.
    let options = args
        .map(|args| launch::load_options(path, args))
        .transpose()?;
    if let Some(options) = &options {
        let mut loaded = boot::open_protocol_exclusive::<LoadedImage>(image)
            .map_err(|e| reason(path, e.status()))?;
        let size = core::mem::size_of_val(options.as_slice());
        // SAFETY: `options` is a null-terminated UCS-2 string of `size`
        // bytes that outlives the image, which runs only inside
        // `start_image` below.
        unsafe { loaded.set_load_options(options.as_ptr().cast(), size as u32) };
    }

    match boot::start_image(image) {
        Err(err) if launch::start_refused(err.status().0) => Err(reason(path, err.status())),
        // The image ran and exited, whatever its exit status.
        _ => Ok(()),
    }
}
