//! Finding the PCI function that provides the framebuffer.

use mikrosboot::display::{PciLocation, only_location};
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol, SearchType};
use uefi::proto::ProtocolPointer;
use uefi::proto::console::gop::GraphicsOutput;
use uefi::proto::device_path::DevicePath;
use uefi::proto::unsafe_protocol;
use uefi::{Handle, Identify, Status};

/// The part of `EFI_PCI_IO_PROTOCOL` used here: the fourteen function
/// pointers before `GetLocation` (`PollMem`, `PollIo`, the reads and
/// writes of `Mem`, `Io` and `Pci`, `CopyMem`, `Map`, `Unmap`,
/// `AllocateBuffer`, `FreeBuffer` and `Flush`), then `GetLocation`.
#[repr(C)]
#[unsafe_protocol("4cf5b200-68b8-4ca5-9eec-b23e3f50029a")]
struct PciIo {
    before: [usize; 14],
    get_location: unsafe extern "efiapi" fn(
        this: *const PciIo,
        segment: *mut usize,
        bus: *mut usize,
        device: *mut usize,
        function: *mut usize,
    ) -> Status,
}

/// Protocol `P` of `handle`, opened without taking it from its driver.
fn get<P: ProtocolPointer + ?Sized>(handle: Handle) -> Option<ScopedProtocol<P>> {
    let params = OpenProtocolParams {
        handle,
        agent: boot::image_handle(),
        controller: None,
    };
    // SAFETY: GetProtocol does not take ownership, so the protocol's driver
    // keeps using it until boot services end.
    unsafe { boot::open_protocol::<P>(params, OpenProtocolAttributes::GetProtocol) }.ok()
}

/// The PCI function of the controller that the device path of `handle`
/// leads to, if it leads to one.
fn on_path(handle: Handle) -> Option<PciLocation> {
    let path = get::<DevicePath>(handle)?;
    let mut rest: &DevicePath = &path;
    let controller = boot::locate_device_path::<PciIo>(&mut rest).ok()?;
    let pci = get::<PciIo>(controller)?;
    let (mut segment, mut bus, mut device, mut function) = (0, 0, 0, 0);
    let this: &PciIo = &pci;
    // SAFETY: the firmware's protocol, called with its own pointer and with
    // out pointers valid for the call.
    let status =
        unsafe { (this.get_location)(this, &mut segment, &mut bus, &mut device, &mut function) };
    if status != Status::SUCCESS {
        return None;
    }
    PciLocation::new(segment, bus, device, function)
}

/// The PCI function that provides the framebuffer at `base`, which the
/// graphics output protocol of `gop` shows: the one that its handle's
/// device path leads to, or else the single one that the device paths of
/// the other handles with a graphics output protocol showing a framebuffer
/// at `base` lead to, such as the outputs behind a console splitter.
pub fn framebuffer_function(gop: Handle, base: u64) -> Option<PciLocation> {
    if let Some(found) = on_path(gop) {
        return Some(found);
    }
    let handles = boot::locate_handle_buffer(SearchType::ByProtocol(&GraphicsOutput::GUID)).ok()?;
    let shows_base = |handle: Handle| {
        get::<GraphicsOutput>(handle)
            .is_some_and(|mut other| other.frame_buffer().as_mut_ptr() as u64 == base)
    };
    only_location(
        handles
            .iter()
            .copied()
            .filter(|&handle| handle != gop && shows_base(handle))
            .filter_map(on_path),
    )
}
